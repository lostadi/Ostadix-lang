#include <iostream>
#include <fstream>
#include <sstream>
#include <string>
#include <set>
#include <cstring>
#include <cstdlib>

extern "C" {
#include "value.h"
#include "parser.h"
}
#include "eval.hpp"

int main(int argc, char *argv[]) {
    if (argc < 2) {
        std::cerr << "usage: O <file.O> [shim_dir]\n"
                  << "example: O examples/hello.O backends\n";
        return 1;
    }

    std::string input_path = argv[1];
    std::string shim_dir = (argc >= 3) ? argv[2] : "backends";

    // Read file
    std::ifstream file(input_path);
    if (!file) {
        std::cerr << "failed to read input file: " << input_path << "\n";
        return 1;
    }
    std::stringstream ss;
    ss << file.rdbuf();
    std::string source = ss.str();

    // Strip shebang
    if (source.size() >= 2 && source[0] == '#' && source[1] == '!') {
        auto nl = source.find('\n');
        if (nl != std::string::npos)
            source = source.substr(nl + 1);
        else
            source.clear();
    }

    // Registered backends
    std::set<std::string> backends = {
        "O", "python", "html", "latex", "markdown", "bash", "shell",
        "rust", "racket", "nix", "nix_expr", "nix_store", "nixos_test", "quote"
    };

    // Build C string set for parser
    StringSet *backend_set = string_set_new();
    for (const auto &b : backends)
        string_set_add(backend_set, b.c_str());

    // Parse
    OParser parser;
    parser_init(&parser, source.c_str(), backend_set);
    ONodeList *nodes = parser_parse(&parser);
    if (!nodes) {
        std::cerr << "failed to parse .O source: " << parser.error_msg << "\n";
        string_set_free(backend_set);
        return 1;
    }

    // Evaluate
    olang::Evaluator evaluator(shim_dir);
    evaluator.set_registered_backends(backends);

    OValue *result = nullptr;
    try {
        result = evaluator.eval_document(nodes);
    } catch (const std::exception &e) {
        std::cerr << "failed to evaluate .O document: " << e.what() << "\n";
        onode_list_free(nodes);
        string_set_free(backend_set);
        return 1;
    }

    // Output
    if (result) {
        if (result->tag == OVAL_STR || result->tag == OVAL_HTML) {
            std::cout << result->data.str_val;
        } else if (result->tag != OVAL_NULL) {
            char *repr = oval_splice_repr(result);
            std::cout << repr << "\n";
            free(repr);
        }
        oval_release(result);
    }

    onode_list_free(nodes);
    string_set_free(backend_set);
    return 0;
}
