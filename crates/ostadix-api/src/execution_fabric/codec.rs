use crate::canonical_cbor::{decode_bounded, encode, DecodeLimits};

use super::protocol::{
    ExecutionCandidateV1, ExecutionCapsuleV1, ExecutionFabricError, Sha256DigestV1,
    MAX_EXECUTION_CANDIDATE_BYTES, MAX_EXECUTION_CAPSULE_BYTES,
};

const MAX_CAPSULE_ITEMS: usize = 100_000;
const MAX_CANDIDATE_ITEMS: usize = 40_000;
const MAX_WIRE_DEPTH: usize = 64;

pub fn encode_execution_capsule_v1(
    capsule: &ExecutionCapsuleV1,
) -> Result<Vec<u8>, ExecutionFabricError> {
    capsule.validate()?;
    encode_bounded("capsule", capsule, MAX_EXECUTION_CAPSULE_BYTES)
}

pub fn decode_execution_capsule_v1(
    bytes: &[u8],
) -> Result<ExecutionCapsuleV1, ExecutionFabricError> {
    let capsule: ExecutionCapsuleV1 = decode_canonical(
        "capsule",
        bytes,
        MAX_EXECUTION_CAPSULE_BYTES,
        MAX_CAPSULE_ITEMS,
    )?;
    capsule.validate()?;
    Ok(capsule)
}

pub fn execution_capsule_sha256_v1(
    capsule: &ExecutionCapsuleV1,
) -> Result<Sha256DigestV1, ExecutionFabricError> {
    capsule.canonical_sha256()
}

pub fn encode_execution_candidate_v1(
    candidate: &ExecutionCandidateV1,
) -> Result<Vec<u8>, ExecutionFabricError> {
    candidate.validate()?;
    encode_bounded("candidate", candidate, MAX_EXECUTION_CANDIDATE_BYTES)
}

pub fn decode_execution_candidate_v1(
    bytes: &[u8],
) -> Result<ExecutionCandidateV1, ExecutionFabricError> {
    let candidate = decode_execution_candidate_representation_v1(bytes)?;
    candidate.validate()?;
    Ok(candidate)
}

/// Decode only the bounded canonical candidate representation.
///
/// Coordinator-side Fabric acceptance deliberately validates attempt,
/// capsule, contract, OWVALUE, content, and deadline semantics in its ordered
/// gates. Keeping this structural decoder separate prevents a gate-17 digest
/// failure from being reported before node authentication at gates 2 and 3.
pub(crate) fn decode_execution_candidate_representation_v1(
    bytes: &[u8],
) -> Result<ExecutionCandidateV1, ExecutionFabricError> {
    let candidate: ExecutionCandidateV1 = decode_canonical(
        "candidate",
        bytes,
        MAX_EXECUTION_CANDIDATE_BYTES,
        MAX_CANDIDATE_ITEMS,
    )?;
    candidate.validate_representation()?;
    Ok(candidate)
}

fn encode_bounded<T: serde::Serialize>(
    kind: &'static str,
    value: &T,
    maximum: usize,
) -> Result<Vec<u8>, ExecutionFabricError> {
    let bytes = encode(value).map_err(|error| {
        ExecutionFabricError::Codec(format!("failed to encode {kind}: {error:#}"))
    })?;
    if bytes.len() > maximum {
        return Err(ExecutionFabricError::RecordTooLarge {
            kind,
            actual: bytes.len(),
            maximum,
        });
    }
    Ok(bytes)
}

fn decode_canonical<T: serde::de::DeserializeOwned + serde::Serialize>(
    kind: &'static str,
    bytes: &[u8],
    maximum: usize,
    max_items: usize,
) -> Result<T, ExecutionFabricError> {
    if bytes.len() > maximum {
        return Err(ExecutionFabricError::RecordTooLarge {
            kind,
            actual: bytes.len(),
            maximum,
        });
    }
    let value = decode_bounded(
        bytes,
        DecodeLimits {
            max_bytes: maximum,
            max_items,
            max_depth: MAX_WIRE_DEPTH,
        },
    )
    .map_err(|error| ExecutionFabricError::Codec(format!("failed to decode {kind}: {error:#}")))?;
    let canonical = encode(&value).map_err(|error| {
        ExecutionFabricError::Codec(format!("failed to re-encode {kind}: {error:#}"))
    })?;
    if canonical != bytes {
        return Err(ExecutionFabricError::NonCanonical { kind });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use anyhow::{bail, Context, Result};
    use num_bigint::BigInt;
    use sha2::{Digest, Sha256};

    use crate::backend_catalog::SpliceRenderer;
    use crate::eval_core::render_with;
    use crate::value::{FloatFormat, OText, OValue};
    use crate::world::{PortableOValue, PortableValueRecord};

    use super::*;
    use crate::execution_fabric::protocol::tests::{digest, fixture_capsule};
    use crate::execution_fabric::{
        CandidateOutcomeV1, CandidateOutputV1, ExecutionCandidateV1, OutputFidelityV1,
        OutputValueKindV1, RendererPartV1, TrustedInlineRendererV1,
    };

    fn fixture_candidate(capsule: &ExecutionCapsuleV1) -> ExecutionCandidateV1 {
        let output = PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8: "hello world".to_string(),
                encoding: Some("utf-8".to_string()),
            })
            .unwrap(),
        );
        ExecutionCandidateV1::new(
            capsule,
            CandidateOutcomeV1::Succeeded {
                output: CandidateOutputV1::new(
                    "result",
                    &output,
                    OutputValueKindV1::Text,
                    OutputFidelityV1::Structural,
                )
                .unwrap(),
            },
            1_999_999_999_999,
        )
        .unwrap()
    }

    fn fixture_loopback_capsule(renderer: TrustedInlineRendererV1) -> ExecutionCapsuleV1 {
        let input = PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8: "world".to_string(),
                encoding: Some("utf-8".to_string()),
            })
            .unwrap(),
        );
        fixture_loopback_capsule_with(
            renderer,
            vec![
                RendererPartV1::literal("hello "),
                RendererPartV1::input("name"),
            ],
            vec![("name".to_string(), input)],
        )
    }

    fn fixture_loopback_capsule_with(
        renderer: TrustedInlineRendererV1,
        parts: Vec<RendererPartV1>,
        inputs: Vec<(String, PortableValueRecord)>,
    ) -> ExecutionCapsuleV1 {
        let region = super::super::SourceClosedRendererV1::new(
            renderer,
            parts,
            digest(3),
            digest(4),
            digest(5),
            digest(6),
        )
        .unwrap();
        ExecutionCapsuleV1::new(
            super::super::AttemptIdV1::new(
                super::super::LogicalTaskIdV1::new(
                    super::super::ExecutionIdV1::new(digest(1)).unwrap(),
                    digest(2),
                )
                .unwrap(),
                1,
            )
            .unwrap(),
            region,
            digest(7),
            super::super::InputManifestV1::new(
                inputs
                    .iter()
                    .map(|(slot, value)| {
                        super::super::InputBindingV1::new(slot.clone(), value).unwrap()
                    })
                    .collect(),
            )
            .unwrap(),
            super::super::OutputContractV1::for_renderer("result", renderer, 4096).unwrap(),
            2_000_000_000_000,
            super::super::ExecutionLimitsV1::new(30_000, 32 * 1024, 4096).unwrap(),
        )
        .unwrap()
    }

    fn candidate_output_as_ovalue(candidate: &ExecutionCandidateV1) -> OValue {
        let CandidateOutcomeV1::Succeeded { output } = candidate.outcome() else {
            panic!("loopback renderer must produce a successful candidate")
        };
        let PortableValueRecord::Core(PortableOValue::Text(text)) =
            output.value().decode().unwrap()
        else {
            panic!("loopback renderer candidate must carry portable text")
        };
        match output.value_kind() {
            OutputValueKindV1::Text => OValue::Text { v: text },
            OutputValueKindV1::Html => OValue::Html { v: text.utf8 },
        }
    }

    fn assert_loopback_equivalent(capsule: &ExecutionCapsuleV1) -> OValue {
        let direct = execute_trusted_renderer(capsule).unwrap();
        let capsule_bytes = encode_execution_capsule_v1(capsule).unwrap();
        let candidate_bytes = execute_capsule_loopback_v1(&capsule_bytes).unwrap();
        let candidate = decode_execution_candidate_v1(&candidate_bytes).unwrap();
        candidate
            .validate_for_coordinator_acceptance(
                capsule,
                unix_ms(unix_time_now().unwrap()).unwrap(),
            )
            .unwrap();
        let CandidateOutcomeV1::Succeeded { output } = candidate.outcome() else {
            panic!("loopback renderer must produce a successful candidate")
        };
        assert_eq!(output.value_kind(), capsule.output().value_kind());
        assert_eq!(output.fidelity(), capsule.output().fidelity());
        let loopback = candidate_output_as_ovalue(&candidate);
        assert_eq!(loopback, direct);
        assert_eq!(loopback.type_name(), direct.type_name());
        assert_eq!(loopback.content_identity(), direct.content_identity());
        loopback
    }

    fn unix_time_now() -> Result<Duration> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("loopback wall clock is before the Unix epoch")
    }

    fn unix_ms(time: Duration) -> Result<u64> {
        u64::try_from(time.as_millis()).context("loopback wall clock exceeds u64 milliseconds")
    }

    fn validate_loopback_timing(
        capsule: &ExecutionCapsuleV1,
        started_unix_ms: u64,
        completed_unix_ms: u64,
        elapsed: Duration,
        deadline_budget: Duration,
    ) -> Result<()> {
        if started_unix_ms == 0 || completed_unix_ms == 0 {
            bail!("loopback timing observations must be nonzero");
        }
        if completed_unix_ms < started_unix_ms {
            bail!("loopback wall clock moved backward during execution");
        }
        if started_unix_ms > capsule.deadline_unix_ms() {
            bail!("execution capsule expired before loopback realization");
        }
        if completed_unix_ms > capsule.deadline_unix_ms() {
            bail!("execution capsule expired during loopback realization");
        }
        if elapsed > Duration::from_millis(capsule.limits().max_runtime_ms()) {
            bail!("loopback realization exceeded the capsule runtime limit");
        }
        if elapsed > deadline_budget {
            bail!("execution capsule expired during the monotonic deadline budget");
        }
        Ok(())
    }

    /// Execute one canonical capsule in-process and return canonical
    /// provisional-candidate bytes. This proof adapter deliberately obtains its
    /// own wall and monotonic time instead of accepting worker-reported timing.
    fn execute_capsule_loopback_v1(capsule_bytes: &[u8]) -> Result<Vec<u8>> {
        let capsule = decode_execution_capsule_v1(capsule_bytes)
            .context("loopback rejected the execution capsule")?;
        let started_wall = unix_time_now()?;
        let started_unix_ms = unix_ms(started_wall)?;
        let deadline_budget = Duration::from_millis(capsule.deadline_unix_ms())
            .checked_sub(started_wall)
            .ok_or_else(|| {
                anyhow::anyhow!("execution capsule expired before loopback realization")
            })?;
        let started = Instant::now();
        let output = execute_trusted_renderer(&capsule)?;
        let elapsed = started.elapsed();
        let completed_unix_ms = unix_ms(unix_time_now()?)?;
        validate_loopback_timing(
            &capsule,
            started_unix_ms,
            completed_unix_ms,
            elapsed,
            deadline_budget,
        )?;
        let (utf8, value_kind) = match output {
            OValue::Text { v } => (v.utf8, OutputValueKindV1::Text),
            OValue::Html { v } => (v, OutputValueKindV1::Html),
            other => bail!(
                "trusted inline loopback produced unsupported output {}",
                other.type_name()
            ),
        };
        if value_kind != capsule.output().value_kind() {
            bail!("trusted inline loopback violated the output value kind");
        }
        let portable = PortableValueRecord::Core(PortableOValue::text(OText {
            utf8,
            encoding: Some("utf-8".to_string()),
        })?);
        let candidate = ExecutionCandidateV1::new(
            &capsule,
            CandidateOutcomeV1::Succeeded {
                output: CandidateOutputV1::new(
                    capsule.output().slot(),
                    &portable,
                    value_kind,
                    capsule.output().fidelity(),
                )?,
            },
            completed_unix_ms,
        )?;
        encode_execution_candidate_v1(&candidate)
            .context("loopback failed to encode its provisional candidate")
    }

    fn execute_trusted_renderer(capsule: &ExecutionCapsuleV1) -> Result<OValue> {
        let renderer = match capsule.region().renderer() {
            TrustedInlineRendererV1::Html => SpliceRenderer::Html,
            TrustedInlineRendererV1::Markdown => SpliceRenderer::Markdown,
            TrustedInlineRendererV1::Latex => SpliceRenderer::Latex,
            TrustedInlineRendererV1::Text => SpliceRenderer::Default,
        };
        let mut rendered = String::new();
        for part in capsule.region().parts() {
            match part {
                RendererPartV1::Literal { utf8 } => rendered.push_str(utf8),
                RendererPartV1::Input { slot } => {
                    let binding = capsule.inputs().binding(slot).ok_or_else(|| {
                        anyhow::anyhow!("validated capsule omitted renderer input slot {slot}")
                    })?;
                    let PortableValueRecord::Core(portable) = binding.value().decode()? else {
                        unreachable!("execution-fabric validation rejects portable extensions")
                    };
                    let value = lower_renderer_value(portable)?;
                    rendered.push_str(&render_with(renderer, &value));
                }
            }
        }
        Ok(match capsule.output().value_kind() {
            OutputValueKindV1::Text => OValue::str_(rendered),
            OutputValueKindV1::Html => OValue::html(rendered),
        })
    }

    fn lower_renderer_value(value: PortableOValue) -> Result<OValue> {
        Ok(match value {
            PortableOValue::Null => OValue::Null,
            PortableOValue::Bool(v) => OValue::Bool { v },
            PortableOValue::Number(v) => OValue::Number { v },
            PortableOValue::Text(v) => OValue::Text { v },
            PortableOValue::Char(scalar) => OValue::Char { scalar },
            PortableOValue::List(values) => OValue::List {
                v: values
                    .into_iter()
                    .map(lower_renderer_value)
                    .collect::<Result<Vec<_>>>()?,
            },
            PortableOValue::Record(fields) => OValue::Object {
                fields: fields
                    .into_iter()
                    .map(|(key, value)| Ok((key, lower_renderer_value(value)?)))
                    .collect::<Result<BTreeMap<_, _>>>()?,
            },
            PortableOValue::Map(entries) => OValue::EntriesMap {
                entries: entries
                    .into_iter()
                    .map(|(key, value)| {
                        Ok((lower_renderer_value(key)?, lower_renderer_value(value)?))
                    })
                    .collect::<Result<Vec<_>>>()?,
            },
            PortableOValue::Bytes(_)
            | PortableOValue::Tagged(_)
            | PortableOValue::CodeRef(_)
            | PortableOValue::ObjectRef(_)
            | PortableOValue::Error(_) => {
                bail!("execution-fabric admitted a non-renderable portable value")
            }
        })
    }

    #[test]
    fn capsule_round_trip_and_digest_are_stable() {
        let capsule = fixture_capsule();
        let bytes = encode_execution_capsule_v1(&capsule).unwrap();
        let decoded = decode_execution_capsule_v1(&bytes).unwrap();
        assert_eq!(decoded, capsule);
        assert_eq!(*decoded.region().expected_oir_sha256(), digest(3));
        assert_eq!(*decoded.region().expected_plan_sha256(), digest(4));
        assert_eq!(*decoded.region().backend_catalog_sha256(), digest(5));
        assert_eq!(*decoded.region().backend_implementation_sha256(), digest(6));
        assert_eq!(
            decoded.region().source_sha256(),
            capsule.region().source_sha256()
        );
        assert_eq!(*decoded.admission_sha256(), digest(7));
        assert_eq!(encode_execution_capsule_v1(&decoded).unwrap(), bytes);
        assert_eq!(
            execution_capsule_sha256_v1(&decoded).unwrap(),
            execution_capsule_sha256_v1(&capsule).unwrap()
        );
        assert_eq!(
            decoded.canonical_sha256().unwrap(),
            execution_capsule_sha256_v1(&capsule).unwrap()
        );
        assert_eq!(bytes.len(), 1367);
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            "89347e0f8d438e641aab20f3fed04560671559a5045b3c36f277873a3d89a1dd"
        );
        assert_eq!(
            hex::encode(execution_capsule_sha256_v1(&capsule).unwrap()),
            "a7d6ebed476c3c140fd7ac3bc91e0123d4512608bf74df1e7a2adbe012f09285"
        );
    }

    #[test]
    fn candidate_round_trip_remains_provisional_and_capsule_bound() {
        let capsule = fixture_capsule();
        let candidate = fixture_candidate(&capsule);
        candidate.validate_against(&capsule).unwrap();
        let bytes = encode_execution_candidate_v1(&candidate).unwrap();
        let decoded = decode_execution_candidate_v1(&bytes).unwrap();
        decoded.validate_against(&capsule).unwrap();
        let capsule_sha256 = execution_capsule_sha256_v1(&capsule).unwrap();
        assert_eq!(decoded.attempt(), capsule.attempt());
        assert_eq!(decoded.capsule_sha256(), &capsule_sha256);
        assert_eq!(decoded.region_sha256(), capsule.region().region_sha256());
        assert_eq!(
            decoded.input_manifest_sha256(),
            capsule.inputs().manifest_sha256()
        );
        assert_eq!(
            decoded.output_contract_sha256(),
            capsule.output().contract_sha256()
        );
        assert_eq!(decoded.completed_unix_ms(), 1_999_999_999_999);
        assert_eq!(bytes.len(), 759);
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            "8393adf9db63b6ba923fb0319fe0fa9475a7bc0572c75ca2682331c537f2088f"
        );

        let mut wrong_capsule = capsule.clone();
        wrong_capsule.admission_sha256 = digest(42);
        assert!(decoded.validate_against(&wrong_capsule).is_err());
    }

    #[test]
    fn nonminimal_integer_encoding_is_rejected_as_noncanonical() {
        let bytes = encode_execution_capsule_v1(&fixture_capsule()).unwrap();
        let needle = b"\x67version\x01";
        let index = bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("fixture contains a version field");
        let mut noncanonical = Vec::with_capacity(bytes.len() + 1);
        noncanonical.extend_from_slice(&bytes[..index + needle.len() - 1]);
        noncanonical.extend_from_slice(&[0x18, 0x01]);
        noncanonical.extend_from_slice(&bytes[index + needle.len()..]);
        assert!(matches!(
            decode_execution_capsule_v1(&noncanonical),
            Err(ExecutionFabricError::NonCanonical { kind: "capsule" })
        ));
    }

    #[test]
    fn malformed_and_oversized_capsules_fail_before_admission() {
        let mut truncated = encode_execution_capsule_v1(&fixture_capsule()).unwrap();
        truncated.pop();
        assert!(decode_execution_capsule_v1(&truncated).is_err());

        let oversized = vec![0u8; MAX_EXECUTION_CAPSULE_BYTES + 1];
        assert!(matches!(
            decode_execution_capsule_v1(&oversized),
            Err(ExecutionFabricError::RecordTooLarge {
                kind: "capsule",
                ..
            })
        ));
    }

    #[test]
    fn trailing_duplicate_deep_and_hostile_cbor_fail_before_admission() {
        let mut trailing = encode_execution_capsule_v1(&fixture_capsule()).unwrap();
        trailing.push(0);
        assert!(decode_execution_capsule_v1(&trailing)
            .unwrap_err()
            .to_string()
            .contains("trailing"));

        let mut duplicate = encode_execution_capsule_v1(&fixture_capsule()).unwrap();
        assert_eq!(duplicate[0], 0xa9, "fixture must remain a nine-field map");
        duplicate[0] = 0xaa;
        duplicate.extend_from_slice(b"\x67version\x01");
        assert!(matches!(
            decode_execution_capsule_v1(&duplicate),
            Err(ExecutionFabricError::NonCanonical { kind: "capsule" })
        ));

        let mut too_deep = vec![0x81; MAX_WIRE_DEPTH + 2];
        too_deep.push(0xf6);
        assert!(decode_execution_capsule_v1(&too_deep)
            .unwrap_err()
            .to_string()
            .contains("nesting depth"));

        let hostile_declared_array = [0x9a, 0xff, 0xff, 0xff, 0xff];
        assert!(decode_execution_capsule_v1(&hostile_declared_array)
            .unwrap_err()
            .to_string()
            .contains("declares"));
    }

    #[test]
    fn mutated_schema_version_and_bound_digests_fail_closed() {
        let mut capsule = fixture_capsule();
        capsule.schema = "ostadix.unknown/v1".to_string();
        assert!(encode_execution_capsule_v1(&capsule).is_err());

        let mut capsule = fixture_capsule();
        capsule.version = 2;
        assert!(encode_execution_capsule_v1(&capsule).is_err());

        for mutation in 0..4 {
            let mut capsule = fixture_capsule();
            match mutation {
                0 => capsule.region.source_sha256 = digest(21),
                1 => capsule.region.region_sha256 = digest(22),
                2 => capsule.inputs.manifest_sha256 = digest(23),
                3 => capsule.output.contract_sha256 = digest(24),
                _ => unreachable!(),
            }
            assert!(encode_execution_capsule_v1(&capsule).is_err());
        }
    }

    #[test]
    fn candidate_tampering_is_rejected_before_acceptance() {
        let capsule = fixture_capsule();
        let mut candidate = fixture_candidate(&capsule);
        candidate.capsule_sha256 = digest(31);
        assert!(candidate.validate_against(&capsule).is_err());

        let mut candidate = fixture_candidate(&capsule);
        candidate.completed_unix_ms = capsule.deadline_unix_ms + 1;
        assert!(candidate.validate_against(&capsule).is_err());

        let mut candidate = fixture_candidate(&capsule);
        let CandidateOutcomeV1::Succeeded { output } = &mut candidate.outcome else {
            unreachable!()
        };
        output.fidelity = OutputFidelityV1::Presentation;
        assert!(candidate.validate_against(&capsule).is_err());

        let candidate = fixture_candidate(&capsule);
        assert_eq!(
            candidate
                .validate_for_coordinator_acceptance(&capsule, capsule.deadline_unix_ms() + 1,)
                .unwrap_err()
                .failure_class(),
            super::super::ExecutionFabricFailureClassV1::InfrastructureAbort
        );
        assert!(candidate
            .validate_for_coordinator_acceptance(&capsule, capsule.deadline_unix_ms() + 1)
            .unwrap_err()
            .to_string()
            .contains("coordinator observed"));
    }

    #[test]
    fn canonical_loopback_matches_the_direct_trusted_text_renderer() {
        let capsule = fixture_loopback_capsule(TrustedInlineRendererV1::Text);
        let expected = format!(
            "hello {}",
            render_with(SpliceRenderer::Default, &OValue::str_("world"))
        );
        assert_eq!(assert_loopback_equivalent(&capsule), OValue::str_(expected));
    }

    #[test]
    fn html_loopback_preserves_roles_order_escaping_and_exact_ovalue() {
        let danger = PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8: "<script>&\"lambda=λ</script>".to_string(),
                encoding: Some("utf-8".to_string()),
            })
            .unwrap(),
        );
        let capsule = fixture_loopback_capsule_with(
            TrustedInlineRendererV1::Html,
            vec![
                RendererPartV1::literal("<p data-role=\"literal\">"),
                RendererPartV1::input("danger"),
                RendererPartV1::literal("</p>"),
            ],
            vec![("danger".to_string(), danger)],
        );
        let loopback = assert_loopback_equivalent(&capsule);
        assert_eq!(
            loopback,
            OValue::html(
                "<p data-role=\"literal\">&lt;script&gt;&amp;&quot;lambda=λ&lt;/script&gt;</p>"
            )
        );
        assert!(!loopback.to_string().contains("<script>"));
    }

    #[test]
    fn admitted_value_matrix_is_exact_across_the_loopback_boundary() {
        let large_integer = (BigInt::from(1_u8) << 255_usize) - BigInt::from(1_u8);
        let nested = PortableOValue::List(vec![
            PortableOValue::Null,
            PortableOValue::Record(vec![
                ("empty-list".to_string(), PortableOValue::List(Vec::new())),
                (
                    "unicode".to_string(),
                    PortableOValue::text(OText {
                        utf8: "Güney Azərbaycan λ 🦉".to_string(),
                        encoding: Some("utf-8".to_string()),
                    })
                    .unwrap(),
                ),
            ]),
            PortableOValue::Map(vec![(
                PortableOValue::Bool(false),
                PortableOValue::Record(Vec::new()),
            )]),
        ]);
        let cases = vec![
            PortableOValue::Null,
            PortableOValue::text(OText {
                utf8: String::new(),
                encoding: Some("utf-8".to_string()),
            })
            .unwrap(),
            nested,
            PortableOValue::integer(-42).unwrap(),
            PortableOValue::integer(large_integer).unwrap(),
            PortableOValue::binary_float(
                FloatFormat::F64,
                (-0.0_f64).to_bits().to_be_bytes().to_vec(),
            )
            .unwrap(),
            PortableOValue::binary_float(FloatFormat::F32, 0x7fc0_1234_u32.to_be_bytes().to_vec())
                .unwrap(),
        ];

        for (index, value) in cases.into_iter().enumerate() {
            let slot = format!("value-{index}");
            let capsule = fixture_loopback_capsule_with(
                TrustedInlineRendererV1::Text,
                vec![
                    RendererPartV1::literal(format!("before-{index}|")),
                    RendererPartV1::input(slot.clone()),
                    RendererPartV1::literal(format!("|after-{index}")),
                ],
                vec![(slot, PortableValueRecord::Core(value))],
            );
            assert_loopback_equivalent(&capsule);
        }
    }

    #[test]
    fn loopback_rejects_expired_capsules_and_excess_runtime() {
        let mut expired = fixture_loopback_capsule(TrustedInlineRendererV1::Text);
        expired.deadline_unix_ms = 1;
        let expired_bytes = encode_execution_capsule_v1(&expired).unwrap();
        assert!(execute_capsule_loopback_v1(&expired_bytes)
            .unwrap_err()
            .to_string()
            .contains("expired before"));

        let capsule = fixture_loopback_capsule(TrustedInlineRendererV1::Text);
        assert!(validate_loopback_timing(
            &capsule,
            1_000,
            1_001,
            Duration::from_millis(capsule.limits().max_runtime_ms()) + Duration::from_nanos(1),
            Duration::from_secs(60),
        )
        .unwrap_err()
        .to_string()
        .contains("runtime limit"));

        assert!(validate_loopback_timing(
            &capsule,
            1_000,
            1_001,
            Duration::from_millis(1) + Duration::from_nanos(1),
            Duration::from_millis(1),
        )
        .unwrap_err()
        .to_string()
        .contains("monotonic deadline budget"));
    }
}
