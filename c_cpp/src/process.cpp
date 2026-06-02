#include "process.hpp"

#include <cerrno>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <string>
#include <unistd.h>
#include <sys/wait.h>

namespace olang {
namespace {

[[noreturn]] void throw_errno(const std::string &message) {
    throw std::runtime_error(message + ": " + std::strerror(errno));
}

[[noreturn]] void throw_with_context(const std::string &context, const std::exception &error) {
    throw std::runtime_error(context + ": " + error.what());
}

std::string truncate_for_error(const std::string &text, std::size_t limit) {
    return text.substr(0, text.size() < limit ? text.size() : limit);
}

bool has_python_suffix(const std::string &path) {
    return path.size() >= 3 && path.compare(path.size() - 3, 3, ".py") == 0;
}

std::string env_label(uint32_t env_id) {
    return env_id == std::numeric_limits<uint32_t>::max()
             ? std::string("*ephemeral*")
             : std::to_string(env_id);
}

OWireCommand make_exec_command(const std::string &code, OValueMap *bindings) {
    OWireCommand cmd{};
    cmd.tag = WIRE_CMD_EXEC;
    cmd.code = const_cast<char *>(code.c_str());
    cmd.bindings = bindings;
    cmd.value = nullptr;
    return cmd;
}

OWireCommand make_ping_command() {
    OWireCommand cmd{};
    cmd.tag = WIRE_CMD_PING;
    cmd.code = nullptr;
    cmd.bindings = nullptr;
    cmd.value = nullptr;
    return cmd;
}

OWireCommand make_cleanup_command() {
    OWireCommand cmd{};
    cmd.tag = WIRE_CMD_CLEANUP;
    cmd.code = nullptr;
    cmd.bindings = nullptr;
    cmd.value = nullptr;
    return cmd;
}

OWireCommand make_eval_result_command(OValue *value) {
    OWireCommand cmd{};
    cmd.tag = WIRE_CMD_EVAL_RESULT;
    cmd.code = nullptr;
    cmd.bindings = nullptr;
    cmd.value = value;
    return cmd;
}

} // namespace

BackendProcess::BackendProcess(const std::string &shim_path)
    : child_pid_(-1),
      stdin_fd_(-1),
      stdout_fd_(-1),
      stdin_file_(nullptr),
      stdout_file_(nullptr),
      alive_(false) {
    int stdin_pipe[2] = {-1, -1};
    int stdout_pipe[2] = {-1, -1};

    if (pipe(stdin_pipe) != 0) {
        throw_errno("failed to spawn backend shim: " + shim_path);
    }
    if (pipe(stdout_pipe) != 0) {
        const int saved_errno = errno;
        close(stdin_pipe[0]);
        close(stdin_pipe[1]);
        errno = saved_errno;
        throw_errno("failed to spawn backend shim: " + shim_path);
    }

    child_pid_ = fork();
    if (child_pid_ < 0) {
        const int saved_errno = errno;
        close(stdin_pipe[0]);
        close(stdin_pipe[1]);
        close(stdout_pipe[0]);
        close(stdout_pipe[1]);
        errno = saved_errno;
        throw_errno("failed to spawn backend shim: " + shim_path);
    }

    if (child_pid_ == 0) {
        if (dup2(stdin_pipe[0], STDIN_FILENO) < 0 || dup2(stdout_pipe[1], STDOUT_FILENO) < 0) {
            std::perror("failed to spawn backend shim");
            _exit(127);
        }

        close(stdin_pipe[0]);
        close(stdin_pipe[1]);
        close(stdout_pipe[0]);
        close(stdout_pipe[1]);

        if (has_python_suffix(shim_path)) {
            char *argv[] = {
                const_cast<char *>("python3"),
                const_cast<char *>(shim_path.c_str()),
                nullptr,
            };
            execvp("python3", argv);
        } else {
            char *argv[] = {
                const_cast<char *>(shim_path.c_str()),
                nullptr,
            };
            execvp(shim_path.c_str(), argv);
        }

        std::perror("failed to spawn backend shim");
        _exit(127);
    }

    close(stdin_pipe[0]);
    close(stdout_pipe[1]);

    stdin_fd_ = stdin_pipe[1];
    stdout_fd_ = stdout_pipe[0];

    stdin_file_ = fdopen(stdin_fd_, "w");
    if (stdin_file_ == nullptr) {
        const int saved_errno = errno;
        close(stdin_fd_);
        close(stdout_fd_);
        kill(child_pid_, SIGKILL);
        waitpid(child_pid_, nullptr, 0);
        errno = saved_errno;
        throw_errno("backend process did not provide stdin");
    }
    stdin_fd_ = -1;

    stdout_file_ = fdopen(stdout_fd_, "r");
    if (stdout_file_ == nullptr) {
        const int saved_errno = errno;
        fclose(stdin_file_);
        close(stdout_fd_);
        kill(child_pid_, SIGKILL);
        waitpid(child_pid_, nullptr, 0);
        stdin_file_ = nullptr;
        errno = saved_errno;
        throw_errno("backend process did not provide stdout");
    }
    stdout_fd_ = -1;

    alive_ = true;
}

BackendProcess::~BackendProcess() {
    if (alive_) {
        try {
            cleanup();
        } catch (...) {
        }
    }
}

void BackendProcess::send_command(const OWireCommand &cmd) {
    if (!alive_ || stdin_file_ == nullptr) {
        throw std::runtime_error("failed to write command to backend stdin");
    }

    char *json = owire_cmd_to_json(&cmd);
    if (json == nullptr) {
        throw std::runtime_error("failed to serialize OWireCommand");
    }

    const int write_status = std::fprintf(stdin_file_, "%s\n", json);
    std::free(json);
    if (write_status < 0) {
        throw std::runtime_error("failed to write command to backend stdin");
    }
    if (std::fflush(stdin_file_) != 0) {
        throw std::runtime_error("failed to flush backend stdin");
    }
}

ExecStep BackendProcess::recv_step() {
    if (!alive_ || stdout_file_ == nullptr) {
        throw std::runtime_error("failed to read response from backend stdout");
    }

    char *line = nullptr;
    std::size_t capacity = 0;
    errno = 0;
    const ssize_t bytes_read = getline(&line, &capacity, stdout_file_);
    if (bytes_read < 0) {
        const int saved_errno = errno;
        std::free(line);
        if (saved_errno == 0) {
            throw std::runtime_error("backend process closed stdout unexpectedly");
        }
        errno = saved_errno;
        throw_errno("failed to read response from backend stdout");
    }
    if (bytes_read == 0) {
        std::free(line);
        throw std::runtime_error("backend process closed stdout unexpectedly");
    }

    std::string response_line(line, static_cast<std::size_t>(bytes_read));
    std::free(line);

    OWireResponse *response = owire_resp_from_json(response_line.c_str());
    if (response == nullptr) {
        throw std::runtime_error("failed to parse backend response: " + response_line);
    }

    ExecStep step{};
    switch (response->tag) {
        case WIRE_RESP_OK:
            step.kind = ExecStepKind::Done;
            step.value = response->value;
            response->value = nullptr;
            break;
        case WIRE_RESP_ERR: {
            const std::string message = response->message == nullptr
                                          ? std::string()
                                          : std::string(response->message);
            owire_resp_free(response);
            throw std::runtime_error(message);
        }
        case WIRE_RESP_EVAL_REQUEST:
            step.kind = ExecStepKind::EvalRequest;
            step.value = nullptr;
            if (response->src != nullptr) {
                step.src = response->src;
            }
            break;
        default:
            owire_resp_free(response);
            throw std::runtime_error("failed to parse backend response: " + response_line);
    }

    owire_resp_free(response);
    return step;
}

void BackendProcess::send_eval_result(OValue *value) {
    const OWireCommand cmd = make_eval_result_command(value);
    send_command(cmd);
}

OValue *BackendProcess::exec(const std::string &code, OValueMap *bindings) {
    const OWireCommand cmd = make_exec_command(code, bindings);
    send_command(cmd);
    const ExecStep step = recv_step();
    if (step.kind == ExecStepKind::Done) {
        return step.value;
    }

    throw std::runtime_error(
        "unexpected eval_request from shim (src: \"" + truncate_for_error(step.src, 60) +
        "\"): O.eval is only supported when the evaluator uses the exec_with_eval_callback path");
}

void BackendProcess::ping() {
    const OWireCommand cmd = make_ping_command();
    send_command(cmd);
    const ExecStep step = recv_step();
    if (step.kind == ExecStepKind::Done) {
        return;
    }

    throw std::runtime_error(
        "unexpected eval_request during ping (src: \"" + truncate_for_error(step.src, 40) + "\")");
}

void BackendProcess::cleanup() {
    if (!alive_) {
        return;
    }

    std::string pending_error;
    try {
        const OWireCommand cmd = make_cleanup_command();
        send_command(cmd);
    } catch (const std::exception &error) {
        pending_error = error.what();
    }

    if (stdin_file_ != nullptr) {
        fclose(stdin_file_);
        stdin_file_ = nullptr;
    } else if (stdin_fd_ >= 0) {
        close(stdin_fd_);
        stdin_fd_ = -1;
    }

    if (stdout_file_ != nullptr) {
        fclose(stdout_file_);
        stdout_file_ = nullptr;
    } else if (stdout_fd_ >= 0) {
        close(stdout_fd_);
        stdout_fd_ = -1;
    }

    if (child_pid_ > 0) {
        if (kill(child_pid_, SIGKILL) != 0 && errno != ESRCH && pending_error.empty()) {
            pending_error = std::string("failed to kill backend process: ") + std::strerror(errno);
        }
        while (waitpid(child_pid_, nullptr, 0) < 0) {
            if (errno != EINTR) {
                if (pending_error.empty()) {
                    pending_error = std::string("failed to wait for backend process: ") + std::strerror(errno);
                }
                break;
            }
        }
    }

    alive_ = false;
    child_pid_ = -1;

    if (!pending_error.empty()) {
        throw std::runtime_error(pending_error);
    }
}

ProcessRegistry::~ProcessRegistry() {
    cleanup_all();
}

void ProcessRegistry::send_exec(const std::string &lang, uint32_t env_id,
                                const std::string &code, OValueMap *bindings,
                                const std::string &shim_path) {
    const RegistryKey key{lang, env_id};
    auto it = registry_.find(key);
    if (it == registry_.end()) {
        try {
            auto process = std::make_unique<BackendProcess>(shim_path);
            try {
                process->ping();
            } catch (const std::exception &error) {
                throw_with_context("backend `" + lang + "` did not respond to health check", error);
            }
            it = registry_.emplace(key, std::move(process)).first;
        } catch (const std::exception &error) {
            throw_with_context("failed to start backend for language `" + lang + "`", error);
        }
    }

    try {
        const OWireCommand cmd = make_exec_command(code, bindings);
        it->second->send_command(cmd);
    } catch (const std::exception &error) {
        throw_with_context("failed to send Exec to backend `" + lang + "`", error);
    }
}

ExecStep ProcessRegistry::recv_exec_step(const std::string &lang, uint32_t env_id) {
    const RegistryKey key{lang, env_id};
    auto it = registry_.find(key);
    if (it == registry_.end()) {
        throw std::runtime_error("no live backend process for `" + lang + "[" + std::to_string(env_id) + "]`");
    }

    try {
        return it->second->recv_step();
    } catch (const std::exception &error) {
        registry_.erase(key);
        throw_with_context("backend `" + lang + "[" + std::to_string(env_id) + "]` recv_step failed", error);
    }
}

void ProcessRegistry::send_eval_result(const std::string &lang, uint32_t env_id, OValue *value) {
    const RegistryKey key{lang, env_id};
    auto it = registry_.find(key);
    if (it == registry_.end()) {
        throw std::runtime_error("no live backend process for `" + lang + "[" + std::to_string(env_id) + "]`");
    }

    try {
        it->second->send_eval_result(value);
    } catch (const std::exception &error) {
        throw_with_context("failed to send eval_result to backend `" + lang + "`", error);
    }
}

OValue *ProcessRegistry::exec(const std::string &lang, uint32_t env_id,
                              const std::string &code, OValueMap *bindings,
                              const std::string &shim_path) {
    const RegistryKey key{lang, env_id};

    auto it = registry_.find(key);
    if (it == registry_.end()) {
        try {
            auto process = std::make_unique<BackendProcess>(shim_path);
            try {
                process->ping();
            } catch (const std::exception &error) {
                throw_with_context("backend `" + lang + "` did not respond to health check", error);
            }
            it = registry_.emplace(key, std::move(process)).first;
        } catch (const std::exception &error) {
            throw_with_context("failed to start backend for language `" + lang + "`", error);
        }
    }

    try {
        return it->second->exec(code, bindings);
    } catch (const std::exception &error) {
        registry_.erase(key);
        throw_with_context("backend `" + lang + "` env [" + env_label(env_id) + "] failed while executing code", error);
    }
}

void ProcessRegistry::cleanup_env(const std::string &lang, uint32_t env_id) {
    const RegistryKey key{lang, env_id};
    auto it = registry_.find(key);
    if (it == registry_.end()) {
        return;
    }

    auto process = std::move(it->second);
    registry_.erase(it);
    process->cleanup();
}

void ProcessRegistry::cleanup_all() {
    while (!registry_.empty()) {
        auto it = registry_.begin();
        std::unique_ptr<BackendProcess> process = std::move(it->second);
        registry_.erase(it);
        if (process != nullptr) {
            try {
                process->cleanup();
            } catch (...) {
            }
        }
    }
}

} // namespace olang
