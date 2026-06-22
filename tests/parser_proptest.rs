use std::collections::HashSet;

use o_lang::parser::Parser;
use proptest::prelude::*;

fn backends() -> HashSet<String> {
    [
        "O",
        "quote",
        "python",
        "bash",
        "html",
        "markdown",
        "nix",
        "nix_store",
        "javascript",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn adversarial_piece() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..64)
            .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        Just("python^(".into()),
        Just(")_python".into()),
        Just("python[429496729999999999999]^(".into()),
        Just("python{defer,cap=runner,process}^(".into()),
        Just("python{cap=,network}^(".into()),
        Just("\\python^(".into()),
        Just("\\)_python".into()),
        Just("$name".into()),
        Just("${name}".into()),
        Just("let x = ".into()),
        Just("# comment\n".into()),
        Just("\0\u{fffd}\r\n".into()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn arbitrary_bytes_never_panic_in_document_parser(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let source = String::from_utf8_lossy(&bytes);
        let _ = Parser::new(&source, &backends()).parse();
    }

    #[test]
    fn adversarial_delimiter_sequences_never_panic(
        pieces in prop::collection::vec(adversarial_piece(), 0..64)
    ) {
        let source = pieces.concat();
        let _ = Parser::new(&source, &backends()).parse();
    }
}
