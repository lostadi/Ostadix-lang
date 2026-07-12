#include "nixos_ops.hpp"

#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <iostream>
#include <map>
#include <stdexcept>
#include <string>
#include <vector>

namespace olang {
namespace {

struct JsonValue {
    enum class Type { Null, Bool, Number, String, Object, Array };
    Type type = Type::Null;
    bool bool_value = false;
    std::string string_value;
    std::map<std::string, JsonValue> object_value;

    const JsonValue *get(const std::string &key) const {
        auto it = object_value.find(key);
        return it == object_value.end() ? nullptr : &it->second;
    }
};

class JsonParser {
public:
    explicit JsonParser(const std::string &text) : text_(text), pos_(0) {}

    JsonValue parse() {
        JsonValue value = parse_value();
        skip_ws();
        if (pos_ != text_.size()) {
            throw std::runtime_error("unexpected trailing JSON content");
        }
        return value;
    }

private:
    JsonValue parse_value() {
        skip_ws();
        if (pos_ >= text_.size()) {
            throw std::runtime_error("unexpected end of JSON");
        }
        const char c = text_[pos_];
        if (c == '{') return parse_object();
        if (c == '[') return parse_array();
        if (c == '"') {
            JsonValue v;
            v.type = JsonValue::Type::String;
            v.string_value = parse_string();
            return v;
        }
        if (c == 't' && match("true")) {
            JsonValue v;
            v.type = JsonValue::Type::Bool;
            v.bool_value = true;
            return v;
        }
        if (c == 'f' && match("false")) {
            JsonValue v;
            v.type = JsonValue::Type::Bool;
            v.bool_value = false;
            return v;
        }
        if (c == 'n' && match("null")) {
            JsonValue v;
            v.type = JsonValue::Type::Null;
            return v;
        }
        if (c == '-' || (c >= '0' && c <= '9')) {
            JsonValue v;
            v.type = JsonValue::Type::Number;
            v.string_value = parse_number();
            return v;
        }
        throw std::runtime_error("invalid JSON value");
    }

    JsonValue parse_object() {
        expect('{');
        JsonValue v;
        v.type = JsonValue::Type::Object;
        skip_ws();
        if (consume('}')) {
            return v;
        }
        while (true) {
            std::string key = parse_string();
            skip_ws();
            expect(':');
            v.object_value.emplace(std::move(key), parse_value());
            skip_ws();
            if (consume('}')) {
                break;
            }
            expect(',');
        }
        return v;
    }

    JsonValue parse_array() {
        expect('[');
        JsonValue v;
        v.type = JsonValue::Type::Array;
        skip_ws();
        if (consume(']')) {
            return v;
        }
        while (true) {
            (void)parse_value();
            skip_ws();
            if (consume(']')) {
                break;
            }
            expect(',');
        }
        return v;
    }

    std::string parse_string() {
        expect('"');
        std::string out;
        while (pos_ < text_.size()) {
            char c = text_[pos_++];
            if (c == '"') {
                return out;
            }
            if (c != '\\') {
                out.push_back(c);
                continue;
            }
            if (pos_ >= text_.size()) {
                throw std::runtime_error("unterminated JSON escape");
            }
            char esc = text_[pos_++];
            switch (esc) {
                case '"': out.push_back('"'); break;
                case '\\': out.push_back('\\'); break;
                case '/': out.push_back('/'); break;
                case 'b': out.push_back('\b'); break;
                case 'f': out.push_back('\f'); break;
                case 'n': out.push_back('\n'); break;
                case 'r': out.push_back('\r'); break;
                case 't': out.push_back('\t'); break;
                case 'u':
                    if (pos_ + 4 > text_.size()) {
                        throw std::runtime_error("invalid unicode escape");
                    }
                    pos_ += 4;
                    out.push_back('?');
                    break;
                default:
                    throw std::runtime_error("unsupported JSON escape");
            }
        }
        throw std::runtime_error("unterminated JSON string");
    }

    std::string parse_number() {
        const std::size_t start = pos_;
        if (text_[pos_] == '-') {
            ++pos_;
        }
        while (pos_ < text_.size() && text_[pos_] >= '0' && text_[pos_] <= '9') {
            ++pos_;
        }
        if (pos_ < text_.size() && text_[pos_] == '.') {
            ++pos_;
            while (pos_ < text_.size() && text_[pos_] >= '0' && text_[pos_] <= '9') {
                ++pos_;
            }
        }
        return text_.substr(start, pos_ - start);
    }

    void skip_ws() {
        while (pos_ < text_.size()) {
            char c = text_[pos_];
            if (c == ' ' || c == '\n' || c == '\r' || c == '\t') {
                ++pos_;
            } else {
                break;
            }
        }
    }

    bool consume(char c) {
        if (pos_ < text_.size() && text_[pos_] == c) {
            ++pos_;
            return true;
        }
        return false;
    }

    void expect(char c) {
        skip_ws();
        if (pos_ >= text_.size() || text_[pos_] != c) {
            throw std::runtime_error("invalid JSON syntax");
        }
        ++pos_;
    }

    bool match(const char *kw) {
        const std::size_t len = std::strlen(kw);
        if (text_.compare(pos_, len, kw) == 0) {
            pos_ += len;
            return true;
        }
        return false;
    }

    const std::string &text_;
    std::size_t pos_;
};

struct CommandResult {
    int exit_code = -1;
};

std::string value_to_json_text(OValue *value) {
    char *json = oval_to_json(value);
    if (json == nullptr) {
        throw std::runtime_error("failed to serialize OValue");
    }
    std::string text(json);
    std::free(json);
    return text;
}

JsonValue value_to_json(OValue *value) {
    return JsonParser(value_to_json_text(value)).parse();
}

std::string extract_type_name(const JsonValue &json) {
    const JsonValue *t = json.get("t");
    return (t != nullptr && t->type == JsonValue::Type::String) ? t->string_value : "unknown";
}

CommandResult run_command(const std::string &program, const std::vector<std::string> &args,
                          const std::vector<std::pair<std::string, std::string>> &env_pairs) {
    pid_t pid = fork();
    if (pid < 0) {
        throw std::runtime_error(std::string("fork failed: ") + std::strerror(errno));
    }
    if (pid == 0) {
        for (const auto &entry : env_pairs) {
            setenv(entry.first.c_str(), entry.second.c_str(), 1);
        }
        std::vector<char *> argv;
        argv.reserve(args.size() + 2);
        argv.push_back(const_cast<char *>(program.c_str()));
        for (const auto &arg : args) {
            argv.push_back(const_cast<char *>(arg.c_str()));
        }
        argv.push_back(nullptr);
        execvp(program.c_str(), argv.data());
        std::perror("execvp");
        _exit(127);
    }

    int status = 0;
    while (waitpid(pid, &status, 0) < 0) {
        if (errno != EINTR) {
            throw std::runtime_error(std::string("waitpid failed: ") + std::strerror(errno));
        }
    }

    CommandResult result;
    if (WIFEXITED(status)) {
        result.exit_code = WEXITSTATUS(status);
    } else if (WIFSIGNALED(status)) {
        result.exit_code = 128 + WTERMSIG(status);
    }
    return result;
}

} // namespace

OValue *activate_nix(OValue *source, const std::string &profile, bool dry_run) {
    JsonValue json = value_to_json(source);
    const std::string type_name = extract_type_name(json);

    std::string store_path;
    if (type_name == "store_path") {
        const JsonValue *path = json.get("path");
        if (path == nullptr || path->type != JsonValue::Type::String) {
            throw std::runtime_error("activate() store_path missing path");
        }
        store_path = path->string_value;
    } else if (type_name == "derivation") {
        const JsonValue *drv_path = json.get("drv_path");
        const std::string label = (drv_path != nullptr && drv_path->type == JsonValue::Type::String)
            ? drv_path->string_value
            : std::string("<unknown>");
        throw std::runtime_error(
            "activate() expected a StorePath (a realised system closure), got a Derivation (" +
            label + "). Realise it first: activate(realise($drv)).");
    } else if (type_name == "nix_expr") {
        throw std::runtime_error(
            "activate() expected a StorePath, got a NixExpr. The full chain is activate(realise(instantiate($expr))).");
    } else {
        throw std::runtime_error("activate() expected a StorePath, got " + type_name);
    }

    const std::filesystem::path switch_bin = std::filesystem::path(store_path) / "bin" / "switch-to-configuration";
    if (!std::filesystem::exists(switch_bin)) {
        throw std::runtime_error(
            "Path " + store_path + " does not contain bin/switch-to-configuration. This doesn't look like a NixOS system closure.");
    }

    if (!dry_run) {
        std::cerr << "activate: real switching requires the Rust runtime's live "
                  << "system_activation capability; forcing dry-activate in the C++ port\n";
    }
    const std::string action = "dry-activate";

    std::cerr << "activate: profile=" << profile
              << " closure=" << store_path
              << " action=" << action << "\n";

    CommandResult result = run_command(
        switch_bin.string(),
        {action},
        {{"NIX_PROFILE", profile}});
    if (result.exit_code != 0) {
        throw std::runtime_error(
            "switch-to-configuration " + action + " exited with status " +
            std::to_string(result.exit_code));
    }

    return oval_system(profile.c_str());
}

} // namespace olang
