#ifndef O_LANG_PROCESS_HPP
#define O_LANG_PROCESS_HPP

#include <cstdint>
#include <map>
#include <memory>
#include <stdexcept>
#include <string>
#include <sys/types.h>
#include <utility>

extern "C" {
#include "value.h"
}

namespace olang {

/* ── ExecStep ─────────────────────────────────────────────────────────── */
enum class ExecStepKind { Done, EvalRequest };

struct ExecStep {
    ExecStepKind kind;
    OValue *value;       // for Done
    std::string src;     // for EvalRequest
};

/* ── BackendProcess ───────────────────────────────────────────────────── */
class BackendProcess {
public:
    explicit BackendProcess(const std::string &shim_path);
    ~BackendProcess();

    // Non-copyable
    BackendProcess(const BackendProcess &) = delete;
    BackendProcess &operator=(const BackendProcess &) = delete;

    void send_command(const OWireCommand &cmd);
    ExecStep recv_step();
    void send_eval_result(OValue *value);
    OValue *exec(const std::string &code, OValueMap *bindings);
    void ping();
    void cleanup();

private:
    pid_t child_pid_;
    int stdin_fd_;
    int stdout_fd_;
    FILE *stdin_file_;
    FILE *stdout_file_;
    bool alive_;
};

/* ── ProcessRegistry ──────────────────────────────────────────────────── */
using RegistryKey = std::pair<std::string, uint32_t>;

class ProcessRegistry {
public:
    ProcessRegistry() = default;
    ~ProcessRegistry();

    // Non-copyable
    ProcessRegistry(const ProcessRegistry &) = delete;
    ProcessRegistry &operator=(const ProcessRegistry &) = delete;

    void send_exec(const std::string &lang, uint32_t env_id,
                   const std::string &code, OValueMap *bindings,
                   const std::string &shim_path);

    ExecStep recv_exec_step(const std::string &lang, uint32_t env_id);

    void send_eval_result(const std::string &lang, uint32_t env_id, OValue *value);

    OValue *exec(const std::string &lang, uint32_t env_id,
                 const std::string &code, OValueMap *bindings,
                 const std::string &shim_path);

    void cleanup_env(const std::string &lang, uint32_t env_id);
    void cleanup_all();

private:
    std::map<RegistryKey, std::unique_ptr<BackendProcess>> registry_;
};

} // namespace olang

#endif
