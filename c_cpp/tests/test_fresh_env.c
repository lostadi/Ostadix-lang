#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "eval.h"
#include "parser.h"
#include "value.h"

static int failures = 0;

static void check(int condition, const char *message) {
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", message);
        failures += 1;
    }
}

static ONodeList *parse_with(OParser *parser, const StringSet *backends,
                             const char *source) {
    parser_init(parser, source, backends);
    return parser_parse(parser);
}

static OValue *eval_source(OEvaluator *evaluator, const StringSet *backends,
                           const char *source) {
    OParser parser;
    ONodeList *nodes = parse_with(&parser, backends, source);
    OValue *value;

    if (nodes == NULL) {
        fprintf(stderr, "FAIL: parse before evaluation: %s\n", parser.error_msg);
        failures += 1;
        return NULL;
    }
    value = olang_evaluator_eval_document(evaluator, nodes);
    onode_list_free(nodes);
    return value;
}

static void test_parser(const StringSet *backends) {
    const char *source =
        "python^(1)_python\n"
        "py[*]^(2)_py[*]\n"
        "python[4294967293]^(3)_python[4294967293]";
    OParser parser;
    ONodeList *nodes = parse_with(&parser, backends, source);

    check(nodes != NULL, "all environment spellings parse");
    if (nodes != NULL) {
        char *round_trip;
        check(nodes->len == 5,
              "three typed expressions and two separator nodes were parsed");
        if (nodes->len == 5) {
            check(nodes->items[0]->data.typed_expr.env_id == OLANG_ENV_EPHEMERAL,
                  "bare environment uses the ephemeral sentinel");
            check(nodes->items[2]->data.typed_expr.env_id == OLANG_ENV_LINKER_ISOLATED,
                  "[*] environment uses the linker-isolated sentinel");
            check(nodes->items[4]->data.typed_expr.env_id == OLANG_ENV_MAX_PERSISTENT,
                  "largest legal numeric environment remains persistent");
        }
        round_trip = reconstruct_source(nodes->items, nodes->len);
        check(round_trip != NULL && strcmp(round_trip, source) == 0,
              "bare, [*], numeric, and alias delimiters round-trip exactly");
        free(round_trip);
        onode_list_free(nodes);
    }

    {
        const char *reserved[] = {
            "python[4294967294]^(1)_python[4294967294]",
            "python[4294967295]^(1)_python[4294967295]",
            NULL,
        };
        size_t i;
        for (i = 0; reserved[i] != NULL; i++) {
            nodes = parse_with(&parser, backends, reserved[i]);
            check(nodes == NULL, "reserved numeric environment is rejected");
            if (nodes != NULL) {
                onode_list_free(nodes);
            }
        }
    }

    {
        const char *mismatched[] = {
            "python[*]^(1)_python",
            "python^(1)_python[*]",
            "python^(1)_python_suffix",
            "python^(1)_python2",
            "python^(1)_python{defer}",
            "python[*]^(1)_python[*]{defer}",
            NULL,
        };
        size_t i;
        for (i = 0; mismatched[i] != NULL; i++) {
            nodes = parse_with(&parser, backends, mismatched[i]);
            check(nodes == NULL, "an extended closer prefix remains literal");
            if (nodes != NULL) {
                onode_list_free(nodes);
            }
        }
    }

    {
        const char *complete[] = {
            "python[*]^(1)_python[*]tail",
            "python{defer}^(1)_python{defer}tail",
            NULL,
        };
        size_t i;
        for (i = 0; complete[i] != NULL; i++) {
            char *round_trip = NULL;
            nodes = parse_with(&parser, backends, complete[i]);
            check(nodes != NULL, "a complete closer may be followed by text");
            if (nodes != NULL) {
                round_trip = reconstruct_source(nodes->items, nodes->len);
                check(round_trip != NULL && strcmp(round_trip, complete[i]) == 0,
                      "complete closer boundaries round-trip exactly");
                free(round_trip);
                onode_list_free(nodes);
            }
        }
    }
}

static void test_evaluator(const StringSet *backends, const char *shim_dir) {
    OEvaluator *evaluator = olang_evaluator_new(shim_dir);
    OValue *value;

    check(evaluator != NULL, "evaluator is created");
    if (evaluator == NULL) {
        return;
    }
    check(olang_evaluator_set_registered(evaluator, backends),
          "test backends are registered");

    value = eval_source(evaluator, backends, "python^(x = 7)_python");
    check(value != NULL && value->tag == OVAL_NULL,
          "bare state write completes before freshness is tested");
    check(!olang_evaluator_had_error(evaluator),
          "bare state write has no hidden evaluator error");
    oval_release(value);
    value = eval_source(
        evaluator, backends,
        "python^(globals().get('x', 'bare-fresh'))_python");
    check(value != NULL && value->tag == OVAL_STR &&
              strcmp(value->data.str_val, "bare-fresh") == 0,
          "bare environment is fresh on the next attempt");
    check(!olang_evaluator_had_error(evaluator),
          "bare freshness read has no hidden evaluator error");
    oval_release(value);

    value = eval_source(evaluator, backends, "python[*]^(y = 8)_python[*]");
    check(value != NULL && value->tag == OVAL_NULL,
          "[*] state write completes before freshness is tested");
    check(!olang_evaluator_had_error(evaluator),
          "[*] state write has no hidden evaluator error");
    oval_release(value);
    value = eval_source(
        evaluator, backends,
        "python[*]^(globals().get('y', 'star-fresh'))_python[*]");
    check(value != NULL && value->tag == OVAL_STR &&
              strcmp(value->data.str_val, "star-fresh") == 0,
          "[*] environment is fresh on the next attempt");
    check(!olang_evaluator_had_error(evaluator),
          "[*] freshness read has no hidden evaluator error");
    oval_release(value);

    value = eval_source(evaluator, backends, "python[17]^(z = 40)_python[17]");
    check(value != NULL && value->tag == OVAL_NULL,
          "numeric state write completes before persistence is tested");
    check(!olang_evaluator_had_error(evaluator),
          "numeric state write has no hidden evaluator error");
    oval_release(value);
    value = eval_source(evaluator, backends, "python[17]^(z + 2)_python[17]");
    check(value != NULL && value->tag == OVAL_INT && value->data.int_val == 42,
          "numeric environment persists across attempts");
    check(!olang_evaluator_had_error(evaluator),
          "numeric persistence read has no hidden evaluator error");
    oval_release(value);

    check(!olang_evaluator_had_error(evaluator), "focused evaluations completed cleanly");
    olang_evaluator_free(evaluator);
}

int main(int argc, char **argv) {
    StringSet *backends;

    if (argc != 2) {
        fprintf(stderr, "usage: %s <shim-dir>\n", argv[0]);
        return 2;
    }
    backends = string_set_new();
    if (backends == NULL) {
        fprintf(stderr, "FAIL: backend set allocation\n");
        return 1;
    }
    string_set_add(backends, "python");
    string_set_add(backends, "py");

    test_parser(backends);
    test_evaluator(backends, argv[1]);
    string_set_free(backends);

    if (failures != 0) {
        fprintf(stderr, "%d focused fresh-environment checks failed\n", failures);
        return 1;
    }
    puts("PASS: C17 [*] parser round-trip and fresh/persistent lifecycle");
    return 0;
}
