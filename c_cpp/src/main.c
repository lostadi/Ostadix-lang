#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "value.h"
#include "parser.h"
#include "eval.h"

static char *read_file(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz < 0) { fclose(f); return NULL; }
    char *buf = (char *)malloc((size_t)sz + 1);
    if (!buf) { fclose(f); return NULL; }
    size_t n = fread(buf, 1, (size_t)sz, f);
    buf[n] = 0;
    fclose(f);
    return buf;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: O <file.O> [shim_dir]\n"
                        "  example: O ../examples/hello.O ../backends\n");
        return 1;
    }
    const char *input = argv[1];
    const char *shim_dir = (argc >= 3) ? argv[2] : "backends";

    char *source = read_file(input);
    if (!source) {
        fprintf(stderr, "failed to read %s\n", input);
        return 1;
    }
    /* strip shebang */
    if (source[0] == '#' && source[1] == '!') {
        char *nl = strchr(source, '\n');
        if (nl) memmove(source, nl + 1, strlen(nl));
        else source[0] = 0;
    }

    /* default registered (same as before) */
    StringSet *bs = string_set_new();
    const char *tags[] = {"O", "python", "html", "latex", "markdown", "bash", "shell",
                          "rust", "racket", "nix", "nix_expr", "nix_store", "nixos_test", "quote", NULL};
    for (int i=0; tags[i]; ++i) string_set_add(bs, tags[i]);

    OParser p;
    parser_init(&p, source, bs);
    ONodeList *nodes = parser_parse(&p);
    if (!nodes) {
        fprintf(stderr, "parse error: %s\n", p.error_msg);
        free(source);
        string_set_free(bs);
        return 1;
    }

    OEvaluator *ev = olang_evaluator_new(shim_dir);
    olang_evaluator_set_registered(ev, bs);

    OValue *result = olang_evaluator_eval_document(ev, nodes);
    /* result may be NULL-ish, but fn returns something */

    if (result) {
        if (result->tag == OVAL_STR || result->tag == OVAL_HTML) {
            const char *s = result->data.str_val ? result->data.str_val : "";
            fputs(s, stdout);
            /* no extra newline if it was html/str passthru */
        } else if (!oval_is_null(result)) {
            char *repr = oval_splice_repr(result);
            puts(repr ? repr : "");
            free(repr);
        }
        oval_release(result);
    }

    onode_list_free(nodes);
    olang_evaluator_free(ev);
    string_set_free(bs);
    free(source);
    return 0;
}
