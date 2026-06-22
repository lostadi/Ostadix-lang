#![no_main]

use std::collections::HashSet;

use libfuzzer_sys::fuzz_target;
use o_lang::parser::Parser;

fuzz_target!(|input: &[u8]| {
    let source = String::from_utf8_lossy(input);
    let backends = [
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
    .collect::<HashSet<_>>();
    let _ = Parser::new(&source, &backends).parse();
});
