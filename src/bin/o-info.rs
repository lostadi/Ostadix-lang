//! Local, authority-free CLI for Information Kernel V1.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use o_lang::information::{
    information_pack_key_id_v1, AcquisitionModalityV1, EntityDescriptorV1, InformationAtomV1,
    InformationDeltaPackV1, InformationDeltaV1, InformationObjectKindV1,
    InformationPackKeyResolverV1, InformationPackSignerV1, InformationRevisionV1,
    InformationSnapshotV1, InformationStoreReaderV1, InformationStoreV1, OfflinePackPolicyV1,
    PackedInformationObjectV1, ParticipantV1, PayloadRefV1, PublicScalarV1, RevisionIdV1, ScopeV1,
    SignedInformationDeltaPackV1, TypedInformationObjectV1, MAX_SIGNED_INFORMATION_PACK_BYTES_V1,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DEFAULT_STATE: &str = ".ostadix-information";
const DEFAULT_HEAD: &str = "main";
const PRIVATE_KEY_SCHEMA_V1: &str = "ostadix.info-private-signing-key/v1";
const TRUST_SCHEMA_V1: &str = "ostadix.info-trust/v1";
const TRUST_PURPOSE_V1: &str = "verify-offline-information-delta-packs-only";
const NON_AUTHORITY_NOTICE: &str =
    "information presence and signatures grant no execution authority";
const MAX_HEAD_INSPECTION_OUTPUT_BYTES_V1: usize = 256 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "o-info",
    version,
    about = "Manage a local authority-free Ostadix information store"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the deterministic empty snapshot and local head.
    Init {
        #[arg(long, default_value = DEFAULT_STATE)]
        state: PathBuf,
        #[arg(long, default_value = DEFAULT_HEAD)]
        head: String,
    },
    /// Generate a private Ed25519 key and a separate public-only trust file.
    Keygen {
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        trust: PathBuf,
    },
    /// Record one declared public T0 scalar and emit its signed offline pack.
    Record {
        #[arg(long, default_value = DEFAULT_STATE)]
        state: PathBuf,
        #[arg(long, default_value = DEFAULT_HEAD)]
        head: String,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        pack: PathBuf,
        #[arg(long)]
        namespace: String,
        #[arg(long)]
        kind: String,
        #[arg(long = "coordinate", required = true)]
        coordinates: Vec<String>,
        #[arg(long)]
        predicate: String,
        #[arg(long, value_enum)]
        scalar: ScalarKind,
        #[arg(long)]
        value: Option<String>,
        /// Confirm that the scalar is intentionally public and contains no secret.
        #[arg(long)]
        acknowledge_public: bool,
    },
    /// Verify canonical bytes and signature against an independent trust file.
    Verify {
        #[arg(long)]
        pack: PathBuf,
        #[arg(long)]
        trust: PathBuf,
    },
    /// Verify and conservatively import; stale/incomplete packs stay historical.
    Import {
        #[arg(long, default_value = DEFAULT_STATE)]
        state: PathBuf,
        #[arg(long, default_value = DEFAULT_HEAD)]
        head: String,
        #[arg(long)]
        pack: PathBuf,
        #[arg(long)]
        trust: PathBuf,
    },
    /// Inspect one local head and its exact immutable snapshot.
    Head {
        #[arg(long, default_value = DEFAULT_STATE)]
        state: PathBuf,
        #[arg(long, default_value = DEFAULT_HEAD)]
        head: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ScalarKind {
    Null,
    Bool,
    I64,
    U64,
    F64Bits,
    Text,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrivateSigningKeyV1 {
    schema: String,
    algorithm: String,
    key_id: String,
    public_key: String,
    secret_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TrustedKeyV1 {
    key_id: String,
    public_key: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TrustFileV1 {
    schema: String,
    purpose: String,
    algorithm: String,
    keys: Vec<TrustedKeyV1>,
}

struct TrustResolverV1 {
    keys: BTreeMap<[u8; 32], [u8; 32]>,
}

impl InformationPackKeyResolverV1 for TrustResolverV1 {
    fn resolve_ed25519(&self, key_id: &[u8; 32]) -> Option<[u8; 32]> {
        self.keys.get(key_id).copied()
    }
}

struct VerifiedInput {
    bytes: Vec<u8>,
    pack_sha256: String,
    trust_sha256: String,
    verified: o_lang::information::VerifiedInformationDeltaPackV1,
}

struct Promotion {
    next: RevisionIdV1,
    objects: Vec<PackedInformationObjectV1>,
    fact_count: usize,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Init { state, head } => init(&state, &head),
        Command::Keygen { key, trust } => keygen(&key, &trust),
        Command::Record {
            state,
            head,
            key,
            pack,
            namespace,
            kind,
            coordinates,
            predicate,
            scalar,
            value,
            acknowledge_public,
        } => record(RecordRequest {
            state,
            head,
            key,
            pack,
            namespace,
            kind,
            coordinates,
            predicate,
            scalar,
            value,
            acknowledge_public,
        }),
        Command::Verify { pack, trust } => verify(&pack, &trust),
        Command::Import {
            state,
            head,
            pack,
            trust,
        } => import(&state, &head, &pack, &trust),
        Command::Head { state, head } => inspect_head(&state, &head),
    }
}

fn init(state: &Path, head: &str) -> Result<()> {
    let store = InformationStoreV1::open(state).context("could not open information store")?;
    let snapshot = InformationSnapshotV1::new(Vec::new());
    let snapshot_id = snapshot.id()?;
    let revision = InformationRevisionV1::new(snapshot_id.clone(), Vec::new(), None)?;
    let revision_id = revision.id()?;

    match store.read_head(head)? {
        None => {
            store.put(
                InformationObjectKindV1::Snapshot,
                snapshot_id.as_sha256(),
                &snapshot,
            )?;
            store.put(
                InformationObjectKindV1::Revision,
                revision_id.as_sha256(),
                &revision,
            )?;
            store.compare_and_set_head(head, None, &revision_id)?;
            println!(
                "initialized state={} head={} revision={} snapshot={} facts=0",
                state.display(),
                head,
                revision_id,
                snapshot_id
            );
        }
        Some(current) if current == revision_id => {
            require_stored_revision(&store, &current)?;
            require_stored_snapshot(&store, &snapshot_id)?;
            println!(
                "already-initialized state={} head={} revision={} snapshot={} facts=0",
                state.display(),
                head,
                revision_id,
                snapshot_id
            );
        }
        Some(current) => bail!(
            "head `{head}` already exists at non-empty revision {current}; refusing to replace it"
        ),
    }
    println!("authority={NON_AUTHORITY_NOTICE}");
    Ok(())
}

fn keygen(key: &Path, trust: &Path) -> Result<()> {
    if key == trust {
        bail!("private key and public trust file must be different paths");
    }
    ensure_new_destination(key)?;
    ensure_new_destination(trust)?;

    let mut secret = [0_u8; 32];
    getrandom::fill(&mut secret).context("could not obtain entropy for Ed25519 key")?;
    let signer = InformationPackSignerV1::from_secret_bytes(secret);
    let key_id = hex::encode(signer.key_id());
    let public_key = hex::encode(signer.public_key());
    let private = PrivateSigningKeyV1 {
        schema: PRIVATE_KEY_SCHEMA_V1.to_string(),
        algorithm: "ed25519".to_string(),
        key_id: key_id.clone(),
        public_key: public_key.clone(),
        secret_key: hex::encode(secret),
    };
    let trust_record = TrustFileV1 {
        schema: TRUST_SCHEMA_V1.to_string(),
        purpose: TRUST_PURPOSE_V1.to_string(),
        algorithm: "ed25519".to_string(),
        keys: vec![TrustedKeyV1 {
            key_id: key_id.clone(),
            public_key,
        }],
    };
    write_new_json(trust, &trust_record)?;
    if let Err(error) = write_new_json(key, &private) {
        let _ = fs::remove_file(trust);
        return Err(error);
    }
    println!(
        "generated key={} trust={} key-id={}",
        key.display(),
        trust.display(),
        key_id
    );
    println!("trust-purpose={TRUST_PURPOSE_V1}");
    Ok(())
}

struct RecordRequest {
    state: PathBuf,
    head: String,
    key: PathBuf,
    pack: PathBuf,
    namespace: String,
    kind: String,
    coordinates: Vec<String>,
    predicate: String,
    scalar: ScalarKind,
    value: Option<String>,
    acknowledge_public: bool,
}

fn record(request: RecordRequest) -> Result<()> {
    if !request.acknowledge_public {
        bail!(
            "record requires --acknowledge-public; o-info cannot detect secrets in arbitrary scalar text"
        );
    }
    let signer = read_signer(&request.key)?;
    let store = InformationStoreV1::open(&request.state)
        .context("could not open local information store")?;
    let base = store
        .read_head(&request.head)?
        .with_context(|| format!("head `{}` does not exist; run o-info init", request.head))?;
    let base_revision = require_stored_revision(&store, &base)?;
    let base_snapshot = require_stored_snapshot(&store, base_revision.snapshot())?;

    let subject = EntityDescriptorV1::new(
        request.namespace,
        request.kind,
        parse_coordinates(&request.coordinates)?,
    )?;
    let producer = EntityDescriptorV1::new(
        "ostadix.info",
        "offline-pack-signer",
        BTreeMap::from([("key-id".to_string(), hex::encode(signer.key_id()))]),
    )?;
    let subject_id = subject.id()?;
    let producer_id = producer.id()?;
    let atom = InformationAtomV1::new(
        vec![ParticipantV1::new("subject", subject_id.clone())?],
        request.predicate,
        PayloadRefV1::public(parse_scalar(request.scalar, request.value.as_deref())?)?,
        AcquisitionModalityV1::Declared,
        ScopeV1::default(),
        producer_id.clone(),
        Vec::new(),
    )?
    .with_transparency_consequences([NON_AUTHORITY_NOTICE.to_string()]);
    let atom_id = atom.id()?;
    let delta =
        InformationDeltaV1::new(base.clone(), producer_id, vec![atom_id.clone()], Vec::new())?;
    let delta_id = delta.id()?;
    let mut facts = base_snapshot.facts().to_vec();
    facts.push(atom_id.clone());
    let snapshot = InformationSnapshotV1::new(facts);
    let snapshot_id = snapshot.id()?;
    let revision = InformationRevisionV1::new(
        snapshot_id.clone(),
        vec![base.clone()],
        Some(delta_reconciliation_identity(&delta_id.to_string())),
    )?;
    let revision_id = revision.id()?;

    let objects = vec![
        PackedInformationObjectV1::from_entity(&subject)?,
        PackedInformationObjectV1::from_entity(&producer)?,
        PackedInformationObjectV1::from_atom(&atom)?,
        PackedInformationObjectV1::from_snapshot(&snapshot)?,
        PackedInformationObjectV1::from_revision(&revision)?,
        PackedInformationObjectV1::from_delta(&delta)?,
    ];
    let pack = InformationDeltaPackV1::new(delta, objects.clone(), OfflinePackPolicyV1::default())?;
    let signed = signer.sign(pack)?;
    let signed_bytes = signed.canonical_bytes()?;
    write_new_file(&request.pack, &signed_bytes)?;

    for object in &objects {
        store.put_canonical(object.kind, &object.sha256, &object.canonical_bytes)?;
    }
    store.compare_and_set_head(&request.head, Some(&base), &revision_id)?;
    println!(
        "recorded state={} head={} base={} revision={} snapshot={} atom={} pack={} pack-sha256={} signer-key-id={}",
        request.state.display(),
        request.head,
        base,
        revision_id,
        snapshot_id,
        atom_id,
        request.pack.display(),
        plain_sha256(&signed_bytes),
        hex::encode(signer.key_id())
    );
    println!("authority={NON_AUTHORITY_NOTICE}");
    Ok(())
}

fn verify(pack: &Path, trust: &Path) -> Result<()> {
    let input = read_and_verify(pack, trust)?;
    println!(
        "verified pack={} pack-sha256={} signer-key-id={} trust-sha256={} base={} additions={}",
        pack.display(),
        input.pack_sha256,
        input.verified.signer_key_id(),
        input.trust_sha256,
        input.verified.pack().delta().base_revision(),
        input.verified.pack().delta().additions().len()
    );
    println!("authority={NON_AUTHORITY_NOTICE}");
    Ok(())
}

fn import(state: &Path, head: &str, pack: &Path, trust: &Path) -> Result<()> {
    let input = read_and_verify(pack, trust)?;
    let store = InformationStoreV1::open(state).context("could not open information store")?;
    match assess_promotion(
        input.verified.pack(),
        input.verified.signer_key_id(),
        store.read_head(head)?,
        &store,
    ) {
        Ok(promotion) => {
            let expected = input.verified.pack().delta().base_revision();
            for object in &promotion.objects {
                store.put_canonical(object.kind, &object.sha256, &object.canonical_bytes)?;
            }
            store.compare_and_set_head(head, Some(expected), &promotion.next)?;
            println!(
                "imported disposition=current-eligible state={} head={} base={} revision={} facts={} pack-sha256={} signer-key-id={}",
                state.display(),
                head,
                expected,
                promotion.next,
                promotion.fact_count,
                input.pack_sha256,
                input.verified.signer_key_id()
            );
        }
        Err(reason) => {
            store.archive_verified_pack(&input.pack_sha256, &input.bytes)?;
            println!(
                "imported disposition=historical-only state={} head={} base={} pack-sha256={} signer-key-id={} reason={}",
                state.display(),
                head,
                input.verified.pack().delta().base_revision(),
                input.pack_sha256,
                input.verified.signer_key_id(),
                single_line(&reason.to_string())
            );
        }
    }
    println!("authority={NON_AUTHORITY_NOTICE}");
    Ok(())
}

fn inspect_head(state: &Path, head: &str) -> Result<()> {
    let store = InformationStoreReaderV1::open_existing(state)
        .context("could not open existing information store for read-only inspection")?;
    let Some(revision_id) = store.read_head(head)? else {
        println!("head state={} name={} revision=none", state.display(), head);
        println!("authority={NON_AUTHORITY_NOTICE}");
        return Ok(());
    };
    let revision = require_readonly_revision(&store, &revision_id)?;
    let snapshot = require_readonly_snapshot(&store, revision.snapshot())?;
    let mut output = format!(
        "head state={} name={} revision={} snapshot={} facts={}",
        state.display(),
        head,
        revision_id,
        revision.snapshot(),
        snapshot.facts().len()
    );
    output.push('\n');
    for fact in snapshot.facts() {
        output.push_str(&format!("fact={fact}\n"));
        if output.len() > MAX_HEAD_INSPECTION_OUTPUT_BYTES_V1 {
            bail!(
                "read-only head inspection exceeds the {} byte output bound",
                MAX_HEAD_INSPECTION_OUTPUT_BYTES_V1
            );
        }
    }
    output.push_str(&format!("authority={NON_AUTHORITY_NOTICE}\n"));
    if output.len() > MAX_HEAD_INSPECTION_OUTPUT_BYTES_V1 {
        bail!(
            "read-only head inspection exceeds the {} byte output bound",
            MAX_HEAD_INSPECTION_OUTPUT_BYTES_V1
        );
    }
    print!("{output}");
    Ok(())
}

fn assess_promotion(
    pack: &InformationDeltaPackV1,
    verified_signer_key_id: &str,
    current: Option<RevisionIdV1>,
    store: &InformationStoreV1,
) -> Result<Promotion> {
    pack.delta().validate()?;
    let current = current.context("local head does not exist")?;
    if &current != pack.delta().base_revision() {
        bail!(
            "exact-base mismatch: local={} pack={}",
            current,
            pack.delta().base_revision()
        );
    }
    if pack.delta().additions().len() != 1 {
        bail!("bounded V1 import requires exactly one added atom");
    }
    if !pack.delta().expected_heads().is_empty() {
        bail!("expected semantic heads cannot yet be proven by the bounded local importer");
    }
    let expected_producer = EntityDescriptorV1::new(
        "ostadix.info",
        "offline-pack-signer",
        BTreeMap::from([("key-id".to_string(), verified_signer_key_id.to_string())]),
    )?;
    let expected_producer_id = expected_producer.id()?;
    if pack.delta().producer() != &expected_producer_id {
        bail!(
            "delta producer {} is not bound to verified signer key {}",
            pack.delta().producer(),
            verified_signer_key_id
        );
    }

    let mut entities = BTreeMap::new();
    let mut atoms = BTreeMap::new();
    let mut snapshots = BTreeMap::new();
    let mut revisions = BTreeMap::new();
    let mut deltas = BTreeMap::new();
    for object in pack.objects() {
        match object.decode_typed()? {
            TypedInformationObjectV1::Entity(value) => {
                insert_unique(&mut entities, value.id()?.to_string(), value)?;
            }
            TypedInformationObjectV1::Atom(value) => {
                insert_unique(&mut atoms, value.id()?.to_string(), value)?;
            }
            TypedInformationObjectV1::Snapshot(value) => {
                insert_unique(&mut snapshots, value.id()?.to_string(), value)?;
            }
            TypedInformationObjectV1::Revision(value) => {
                insert_unique(&mut revisions, value.id()?.to_string(), value)?;
            }
            TypedInformationObjectV1::Delta(value) => {
                insert_unique(&mut deltas, value.id()?.to_string(), value)?;
            }
        }
    }

    let delta_id = pack.delta().id()?.to_string();
    if deltas.len() != 1 || deltas.get(&delta_id) != Some(pack.delta()) {
        bail!("pack must contain exactly its fully typed inline delta object");
    }
    let addition_ids = pack
        .delta()
        .additions()
        .iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    if atoms.keys().cloned().collect::<BTreeSet<_>>() != addition_ids {
        bail!("pack atom objects do not exactly match delta additions");
    }
    let atom = atoms.values().next().context("pack has no added atom")?;
    if atom.modality() != AcquisitionModalityV1::Declared
        || !matches!(atom.payload(), PayloadRefV1::T0(_))
        || !atom.support().is_empty()
    {
        bail!("bounded V1 import accepts only one support-free declared T0 atom");
    }
    if atom.producer() != pack.delta().producer() {
        bail!("atom producer does not match delta producer");
    }
    if atom.participants().len() != 1 || atom.participants()[0].role != "subject" {
        bail!("bounded V1 atom must have exactly one subject participant");
    }
    let referenced_entities = [
        atom.producer().to_string(),
        atom.participants()[0].entity.to_string(),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    if entities.keys().cloned().collect::<BTreeSet<_>>() != referenced_entities {
        bail!("pack entity objects do not exactly cover atom producer and subject");
    }
    if entities.get(expected_producer_id.as_sha256()) != Some(&expected_producer) {
        bail!("pack producer entity does not describe the verified signer key");
    }

    let base_revision = require_stored_revision(store, &current)?;
    let base_snapshot = require_stored_snapshot(store, base_revision.snapshot())?;
    let mut facts = base_snapshot.facts().to_vec();
    facts.extend(pack.delta().additions().iter().cloned());
    let expected_snapshot = InformationSnapshotV1::new(facts);
    let expected_snapshot_id = expected_snapshot.id()?.to_string();
    if snapshots.len() != 1 || snapshots.get(&expected_snapshot_id) != Some(&expected_snapshot) {
        bail!("pack does not contain the exact base-plus-additions snapshot");
    }
    let expected_revision = InformationRevisionV1::new(
        expected_snapshot.id()?,
        vec![current],
        Some(delta_reconciliation_identity(&delta_id)),
    )?;
    let expected_revision_id = expected_revision.id()?;
    if revisions.len() != 1
        || revisions.get(expected_revision_id.as_sha256()) != Some(&expected_revision)
    {
        bail!("pack does not contain the exact deterministic child revision");
    }

    Ok(Promotion {
        next: expected_revision_id,
        objects: pack.objects().to_vec(),
        fact_count: expected_snapshot.facts().len(),
    })
}

fn require_stored_revision(
    store: &InformationStoreV1,
    id: &RevisionIdV1,
) -> Result<InformationRevisionV1> {
    let object = PackedInformationObjectV1 {
        kind: InformationObjectKindV1::Revision,
        sha256: id.to_string(),
        canonical_bytes: store.get(InformationObjectKindV1::Revision, id.as_sha256())?,
    };
    match object.decode_typed()? {
        TypedInformationObjectV1::Revision(value) => Ok(value),
        _ => unreachable!("revision kind decoded to a different typed object"),
    }
}

fn require_stored_snapshot(
    store: &InformationStoreV1,
    id: &o_lang::information::SnapshotRootIdV1,
) -> Result<InformationSnapshotV1> {
    let object = PackedInformationObjectV1 {
        kind: InformationObjectKindV1::Snapshot,
        sha256: id.to_string(),
        canonical_bytes: store.get(InformationObjectKindV1::Snapshot, id.as_sha256())?,
    };
    match object.decode_typed()? {
        TypedInformationObjectV1::Snapshot(value) => Ok(value),
        _ => unreachable!("snapshot kind decoded to a different typed object"),
    }
}

fn require_readonly_revision(
    store: &InformationStoreReaderV1,
    id: &RevisionIdV1,
) -> Result<InformationRevisionV1> {
    let object = PackedInformationObjectV1 {
        kind: InformationObjectKindV1::Revision,
        sha256: id.to_string(),
        canonical_bytes: store.get(InformationObjectKindV1::Revision, id.as_sha256())?,
    };
    match object.decode_typed()? {
        TypedInformationObjectV1::Revision(value) => Ok(value),
        _ => unreachable!("revision kind decoded to a different typed object"),
    }
}

fn require_readonly_snapshot(
    store: &InformationStoreReaderV1,
    id: &o_lang::information::SnapshotRootIdV1,
) -> Result<InformationSnapshotV1> {
    let object = PackedInformationObjectV1 {
        kind: InformationObjectKindV1::Snapshot,
        sha256: id.to_string(),
        canonical_bytes: store.get(InformationObjectKindV1::Snapshot, id.as_sha256())?,
    };
    match object.decode_typed()? {
        TypedInformationObjectV1::Snapshot(value) => Ok(value),
        _ => unreachable!("snapshot kind decoded to a different typed object"),
    }
}

fn read_and_verify(pack: &Path, trust: &Path) -> Result<VerifiedInput> {
    let bytes = read_regular_file(pack, MAX_SIGNED_INFORMATION_PACK_BYTES_V1)?;
    let signed = SignedInformationDeltaPackV1::decode_canonical(&bytes)
        .context("pack is not canonical InformationDeltaPackV1 CBOR")?;
    let (resolver, trust_sha256) = read_trust(trust)?;
    let verified = signed
        .verify(
            &resolver,
            OfflinePackPolicyV1::default(),
            trust_sha256.clone(),
        )
        .context("pack signature or independent trust policy is invalid")?;
    Ok(VerifiedInput {
        pack_sha256: plain_sha256(&bytes),
        bytes,
        trust_sha256,
        verified,
    })
}

fn read_signer(path: &Path) -> Result<InformationPackSignerV1> {
    let bytes = read_private_regular_file(path, 64 * 1024)?;
    let record: PrivateSigningKeyV1 =
        serde_json::from_slice(&bytes).context("private information key is invalid JSON")?;
    if record.schema != PRIVATE_KEY_SCHEMA_V1 || record.algorithm != "ed25519" {
        bail!("unsupported private information signing-key schema or algorithm");
    }
    let secret = decode_hex::<32>("secret_key", &record.secret_key)?;
    let signer = InformationPackSignerV1::from_secret_bytes(secret);
    if record.public_key != hex::encode(signer.public_key())
        || record.key_id != hex::encode(signer.key_id())
    {
        bail!("private key public identity does not match its secret bytes");
    }
    Ok(signer)
}

fn read_trust(path: &Path) -> Result<(TrustResolverV1, String)> {
    let bytes = read_regular_file(path, 1024 * 1024)?;
    let trust: TrustFileV1 =
        serde_json::from_slice(&bytes).context("information trust file is invalid JSON")?;
    if trust.schema != TRUST_SCHEMA_V1
        || trust.purpose != TRUST_PURPOSE_V1
        || trust.algorithm != "ed25519"
        || trust.keys.is_empty()
    {
        bail!("unsupported or empty information trust policy");
    }
    let mut keys = BTreeMap::new();
    for entry in trust.keys {
        let public = decode_hex::<32>("public_key", &entry.public_key)?;
        let key_id = decode_hex::<32>("key_id", &entry.key_id)?;
        if information_pack_key_id_v1(&public) != key_id {
            bail!("trust key identifier does not match its public key");
        }
        if keys.insert(key_id, public).is_some() {
            bail!("trust policy repeats a signer key identifier");
        }
    }
    Ok((TrustResolverV1 { keys }, plain_sha256(&bytes)))
}

fn parse_coordinates(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut coordinates = BTreeMap::new();
    for value in values {
        let (key, coordinate) = value
            .split_once('=')
            .with_context(|| format!("coordinate `{value}` must be KEY=VALUE"))?;
        if coordinates
            .insert(key.to_string(), coordinate.to_string())
            .is_some()
        {
            bail!("coordinate key `{key}` was supplied more than once");
        }
    }
    Ok(coordinates)
}

fn parse_scalar(kind: ScalarKind, value: Option<&str>) -> Result<PublicScalarV1> {
    match (kind, value) {
        (ScalarKind::Null, None) => Ok(PublicScalarV1::Null),
        (ScalarKind::Null, Some(_)) => bail!("--scalar null must not include --value"),
        (_, None) => bail!("--value is required for this scalar kind"),
        (ScalarKind::Bool, Some("true")) => Ok(PublicScalarV1::Bool(true)),
        (ScalarKind::Bool, Some("false")) => Ok(PublicScalarV1::Bool(false)),
        (ScalarKind::Bool, Some(value)) => {
            bail!("boolean value must be true or false, got `{value}`")
        }
        (ScalarKind::I64, Some(value)) => Ok(PublicScalarV1::I64(
            value.parse().context("--value is not an i64")?,
        )),
        (ScalarKind::U64, Some(value)) => Ok(PublicScalarV1::U64(
            value.parse().context("--value is not a u64")?,
        )),
        (ScalarKind::F64Bits, Some(value)) => {
            let bits = value
                .strip_prefix("0x")
                .map(|hex| u64::from_str_radix(hex, 16))
                .unwrap_or_else(|| value.parse())
                .context("--value is not decimal or 0x-prefixed f64 bits")?;
            Ok(PublicScalarV1::F64Bits(bits))
        }
        (ScalarKind::Text, Some(value)) => Ok(PublicScalarV1::Text(value.to_string())),
    }
}

fn insert_unique<T>(map: &mut BTreeMap<String, T>, key: String, value: T) -> Result<()> {
    if map.insert(key.clone(), value).is_some() {
        bail!("pack repeats typed object identity {key}");
    }
    Ok(())
}

fn delta_reconciliation_identity(delta_id: &str) -> String {
    format!("ostadix.info-delta:{delta_id}")
}

fn decode_hex<const N: usize>(label: &str, encoded: &str) -> Result<[u8; N]> {
    if encoded.len() != N * 2
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} must be lowercase hexadecimal with {N} bytes");
    }
    let bytes = hex::decode(encoded).with_context(|| format!("{label} is not hexadecimal"))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!("{label} has {} bytes; expected {N}", bytes.len())
    })
}

fn plain_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn write_new_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_new_file(path, &bytes)
}

fn ensure_new_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("refusing to overwrite existing path {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not inspect {}", path.display())),
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure_new_destination(path)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("could not write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("could not sync {}", path.display()))?;
    Ok(())
}

fn read_regular_file(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    read_regular_file_with_policy(path, maximum, false)
}

fn read_private_regular_file(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    read_regular_file_with_policy(path, maximum, true)
}

fn read_regular_file_with_policy(
    path: &Path,
    maximum: usize,
    require_private: bool,
) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("could not open {}", path.display()))?;
    let open_metadata = file
        .metadata()
        .with_context(|| format!("could not inspect open file {}", path.display()))?;
    if !open_metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    #[cfg(unix)]
    if require_private {
        use std::os::unix::fs::PermissionsExt;
        if open_metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "private key {} must not be accessible by group or others",
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = require_private;
    if open_metadata.len() > maximum as u64 {
        bail!(
            "{} exceeds the {} byte local limit",
            path.display(),
            maximum
        );
    }
    let read_limit = u64::try_from(maximum)
        .context("local file-size limit does not fit in u64")?
        .checked_add(1)
        .context("local file-size limit overflow")?;
    let initial_capacity = usize::try_from(open_metadata.len())
        .unwrap_or(maximum)
        .min(maximum);
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {}", path.display()))?;
    if bytes.len() > maximum {
        bail!(
            "{} exceeds the {} byte local limit",
            path.display(),
            maximum
        );
    }
    Ok(bytes)
}
