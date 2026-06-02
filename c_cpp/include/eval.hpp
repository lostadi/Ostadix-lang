#ifndef O_LANG_EVAL_HPP
#define O_LANG_EVAL_HPP

#include <cstddef>
#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <set>
#include <string>
#include <utility>
#include <vector>

extern "C" {
#if __has_include("value.h")
#include "value.h"
#else
#define OLANG_EVAL_FALLBACK_VALUE_H 1

typedef enum ORequestKindTag {
    OREQ_INSTANTIATE = 0,
    OREQ_REALISE = 1,
    OREQ_EVAL = 2,
    OREQ_ACTIVATE = 3
} ORequestKindTag;

typedef struct ORequestKind {
    ORequestKindTag tag;
    char *lang;
    uint32_t env_id;
    bool cacheable;
    char *profile;
    bool dry_run;
} ORequestKind;

typedef struct OValue OValue;

typedef struct OValueList {
    OValue **items;
    size_t len;
} OValueList;

typedef struct OMapEntry {
    char *key;
    OValue *value;
} OMapEntry;

typedef struct OValueMap {
    OMapEntry *entries;
    size_t len;
} OValueMap;

typedef struct OBlob {
    char *base64;
    char *mime;
} OBlob;

typedef struct ONixExpr {
    char *body;
    OValue **deps;
    size_t deps_len;
    char *fingerprint;
} ONixExpr;

typedef struct ODerivation {
    char *drv_path;
    char **outputs;
    size_t outputs_len;
    OValue **deps;
    size_t deps_len;
} ODerivation;

typedef struct ORequest {
    ORequestKind kind;
    OValue *source;
    char *fingerprint;
} ORequest;

typedef struct OThunk {
    char *body;
    OValue **deps;
    size_t deps_len;
    char *fingerprint;
} OThunk;

typedef enum OValueTag {
    OVAL_NULL = 0,
    OVAL_BOOL = 1,
    OVAL_INT = 2,
    OVAL_FLOAT = 3,
    OVAL_STR = 4,
    OVAL_HTML = 5,
    OVAL_STORE_PATH = 6,
    OVAL_EXPR = 7,
    OVAL_LIST = 8,
    OVAL_MAP = 9,
    OVAL_BLOB = 10,
    OVAL_NIX_EXPR = 11,
    OVAL_DERIVATION = 12,
    OVAL_REQUEST = 13,
    OVAL_SYSTEM = 14,
    OVAL_THUNK = 15
} OValueTag;

struct OValue {
    OValueTag tag;
    std::size_t refcount;
    union {
        bool bool_v;
        long long int_v;
        double float_v;
        char *str_v;
        OValueList list_v;
        OValueMap map_v;
        OBlob blob_v;
        ONixExpr nix_expr_v;
        ODerivation derivation_v;
        ORequest request_v;
        char *system_profile_v;
        OThunk thunk_v;
    } as;
};
#endif

#if __has_include("parser.h")
#include "parser.h"
#else
#define OLANG_EVAL_FALLBACK_PARSER_H 1

typedef struct ONode ONode;

typedef struct ONodeList {
    ONode **items;
    size_t len;
} ONodeList;

typedef enum ONodeTag {
    ONODE_RAW_TEXT = 0,
    ONODE_VAR_REF = 1,
    ONODE_LET_BINDING = 2,
    ONODE_TYPED_EXPR = 3,
    ONODE_CALL = 4
} ONodeTag;

typedef struct OLetBinding {
    char *name;
    ONode *expr;
} OLetBinding;

typedef struct OTypedExpr {
    char *lang;
    uint32_t env_id;
    char *attr;
    ONode **body;
    size_t body_len;
} OTypedExpr;

typedef struct OCall {
    char *fn_name;
    ONode **args;
    size_t args_len;
} OCall;

struct ONode {
    ONodeTag tag;
    union {
        char *text;
        char *name;
        OLetBinding let_binding;
        OTypedExpr typed_expr;
        OCall call;
    } as;
};
#endif
}

#if __has_include("process.hpp")
#include "process.hpp"
#else
#define OLANG_EVAL_FALLBACK_PROCESS_HPP 1
namespace olang {

struct ExecStep {
    enum class Kind { Done, EvalRequest } kind{Kind::Done};
    OValue *value{nullptr};
    std::string src;

    static ExecStep done(OValue *v) {
        ExecStep step;
        step.kind = Kind::Done;
        step.value = v;
        return step;
    }

    static ExecStep eval_request(const std::string &s) {
        ExecStep step;
        step.kind = Kind::EvalRequest;
        step.src = s;
        return step;
    }
};

class ProcessRegistry {
public:
    ProcessRegistry();
    ~ProcessRegistry();

    OValue *exec(const std::string &lang, uint32_t env_id, const std::string &code,
                 const std::map<std::string, OValue *> &bindings,
                 const std::string &shim_path);
    void send_exec(const std::string &lang, uint32_t env_id, const std::string &code,
                   const std::map<std::string, OValue *> &bindings,
                   const std::string &shim_path);
    ExecStep recv_exec_step(const std::string &lang, uint32_t env_id);
    void send_eval_result(const std::string &lang, uint32_t env_id, OValue *value);
    void cleanup_env(const std::string &lang, uint32_t env_id);
    void cleanup_all();
};

} // namespace olang
#endif

#if __has_include("scheduler.hpp")
#include "scheduler.hpp"
#else
#define OLANG_EVAL_FALLBACK_SCHEDULER_HPP 1
namespace olang {

class AutonomousScheduler {
public:
    AutonomousScheduler();
    ~AutonomousScheduler();

    OValue *execute(OValue *req);
    std::map<std::string, OValue *> execute_batch(
        const std::vector<OValue *> &roots,
        std::function<OValue *(OValue *)> *eval_fn = nullptr);
    OValue *cache_get(const std::string &fingerprint);
    void cache_put(const std::string &fingerprint, OValue *value);

private:
    std::map<std::string, OValue *> mem_cache_;
};

} // namespace olang
#endif

namespace olang {

enum class Policy { Eager, Lazy, Autonomous };

class Executor {
public:
    virtual ~Executor() = default;
    virtual OValue *execute(OValue *req) = 0;
};

class ImmediateExecutor : public Executor {
public:
    ImmediateExecutor() = default;
    OValue *execute(OValue *req) override;
    void seed_cache(const std::string &fingerprint, OValue *value);
private:
    std::map<std::string, OValue *> cache_;
};

class Evaluator {
public:
    explicit Evaluator(const std::string &shim_dir);
    ~Evaluator();

    Evaluator(const Evaluator &) = delete;
    Evaluator &operator=(const Evaluator &) = delete;

    void set_registered_backends(const std::set<std::string> &backends);
    void set_executor(std::unique_ptr<Executor> exec);

    OValue *eval_document(ONodeList *nodes);

private:
    ProcessRegistry registry_;
    std::string shim_dir_;
    std::set<std::string> registered_backends_;
    Policy policy_;
    std::unique_ptr<Executor> executor_;
    std::map<std::string, OValue *> eval_cache_;
    AutonomousScheduler scheduler_;
    std::vector<OValue *> autonomous_buffer_;

    OValue *eval_node(ONode *node, std::map<std::string, OValue *> &scope);
    OValue *eval_call(const std::string &fn_name, ONode **args, size_t args_len,
                      std::map<std::string, OValue *> &scope);
    OValue *eval_typed_expr(const std::string &lang, uint32_t env_id,
                            const char *attr, ONode **body, size_t body_len,
                            std::map<std::string, OValue *> &scope);
    OValue *eval_source(const std::string &src);

    OValue *auto_resolve(OValue *v);
    OValue *force_request(OValue *req);
    void flush_autonomous_buffer();
    OValue *resolve_from_cache(OValue *v);
    OValue *resolve_for_splice(OValue *v);
    OValue *exec_eval(OValue *req);

    std::string render_child(const std::string &lang, OValue *val);
    std::string find_shim(const std::string &lang);

    static bool is_schedulable_request(OValue *v);
    static bool is_pure_backend(const std::string &lang);
};

} // namespace olang

#endif
