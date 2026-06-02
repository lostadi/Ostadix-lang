#include "nix_ops.hpp"

#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cctype>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace olang {
namespace {

struct JsonValue {
    enum class Type { Null, Bool, Number, String, Object, Array };
    Type type = Type::Null;
    bool bool_value = false;
    std::string string_value;
    std::map<std::string, JsonValue> object_value;
    std::vector<JsonValue> array_value;

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
            skip_ws();
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
            v.array_value.push_back(parse_value());
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
                case 'u': {
                    if (pos_ + 4 > text_.size()) {
                        throw std::runtime_error("invalid unicode escape");
                    }
                    const std::string hex = text_.substr(pos_, 4);
                    pos_ += 4;
                    unsigned int code = static_cast<unsigned int>(std::strtoul(hex.c_str(), nullptr, 16));
                    if (code <= 0x7F) {
                        out.push_back(static_cast<char>(code));
                    } else if (code <= 0x7FF) {
                        out.push_back(static_cast<char>(0xC0 | ((code >> 6) & 0x1F)));
                        out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
                    } else {
                        out.push_back(static_cast<char>(0xE0 | ((code >> 12) & 0x0F)));
                        out.push_back(static_cast<char>(0x80 | ((code >> 6) & 0x3F)));
                        out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
                    }
                    break;
                }
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
        if (pos_ < text_.size() && (text_[pos_] == 'e' || text_[pos_] == 'E')) {
            ++pos_;
            if (pos_ < text_.size() && (text_[pos_] == '+' || text_[pos_] == '-')) {
                ++pos_;
            }
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
    std::string out;
    std::string err;
};

std::string read_fd_all(int fd) {
    std::string out;
    char buffer[4096];
    while (true) {
        ssize_t n = read(fd, buffer, sizeof(buffer));
        if (n == 0) {
            break;
        }
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            throw std::runtime_error(std::string("read failed: ") + std::strerror(errno));
        }
        out.append(buffer, static_cast<std::size_t>(n));
    }
    return out;
}

CommandResult run_command(const std::vector<std::string> &args) {
    if (args.empty()) {
        throw std::runtime_error("run_command called with empty argv");
    }

    int stdout_pipe[2] = {-1, -1};
    int stderr_pipe[2] = {-1, -1};
    if (pipe(stdout_pipe) != 0 || pipe(stderr_pipe) != 0) {
        throw std::runtime_error(std::string("pipe failed: ") + std::strerror(errno));
    }

    pid_t pid = fork();
    if (pid < 0) {
        const std::string message = std::string("fork failed: ") + std::strerror(errno);
        close(stdout_pipe[0]); close(stdout_pipe[1]);
        close(stderr_pipe[0]); close(stderr_pipe[1]);
        throw std::runtime_error(message);
    }

    if (pid == 0) {
        dup2(stdout_pipe[1], STDOUT_FILENO);
        dup2(stderr_pipe[1], STDERR_FILENO);
        close(stdout_pipe[0]); close(stdout_pipe[1]);
        close(stderr_pipe[0]); close(stderr_pipe[1]);

        std::vector<char *> argv;
        argv.reserve(args.size() + 1);
        for (const auto &arg : args) {
            argv.push_back(const_cast<char *>(arg.c_str()));
        }
        argv.push_back(nullptr);
        execvp(argv[0], argv.data());
        std::perror("execvp");
        _exit(127);
    }

    close(stdout_pipe[1]);
    close(stderr_pipe[1]);

    CommandResult result;
    result.out = read_fd_all(stdout_pipe[0]);
    result.err = read_fd_all(stderr_pipe[0]);
    close(stdout_pipe[0]);
    close(stderr_pipe[0]);

    int status = 0;
    while (waitpid(pid, &status, 0) < 0) {
        if (errno != EINTR) {
            throw std::runtime_error(std::string("waitpid failed: ") + std::strerror(errno));
        }
    }

    if (WIFEXITED(status)) {
        result.exit_code = WEXITSTATUS(status);
    } else if (WIFSIGNALED(status)) {
        result.exit_code = 128 + WTERMSIG(status);
    } else {
        result.exit_code = -1;
    }
    return result;
}

JsonValue value_to_json(OValue *value) {
    char *json = oval_to_json(value);
    if (json == nullptr) {
        throw std::runtime_error("failed to serialize OValue");
    }
    std::string text(json);
    std::free(json);
    return JsonParser(text).parse();
}

std::string json_escape(const std::string &text) {
    std::string out;
    out.reserve(text.size() + 8);
    for (char c : text) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\b': out += "\\b"; break;
            case '\f': out += "\\f"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (static_cast<unsigned char>(c) < 0x20) {
                    char buf[7];
                    std::snprintf(buf, sizeof(buf), "\\u%04x", static_cast<unsigned char>(c));
                    out += buf;
                } else {
                    out.push_back(c);
                }
        }
    }
    return out;
}

std::string json_dump(const JsonValue &value) {
    switch (value.type) {
        case JsonValue::Type::Null:
            return "null";
        case JsonValue::Type::Bool:
            return value.bool_value ? "true" : "false";
        case JsonValue::Type::Number:
            return value.string_value;
        case JsonValue::Type::String:
            return std::string("\"") + json_escape(value.string_value) + "\"";
        case JsonValue::Type::Array: {
            std::string out = "[";
            for (std::size_t i = 0; i < value.array_value.size(); ++i) {
                if (i != 0) out += ',';
                out += json_dump(value.array_value[i]);
            }
            out += ']';
            return out;
        }
        case JsonValue::Type::Object: {
            std::string out = "{";
            bool first = true;
            for (const auto &entry : value.object_value) {
                if (!first) out += ',';
                first = false;
                out += '"' + json_escape(entry.first) + '"';
                out += ':';
                out += json_dump(entry.second);
            }
            out += '}';
            return out;
        }
    }
    throw std::runtime_error("unknown JSON type");
}

std::vector<OValue *> extract_deps(const JsonValue *deps_value) {
    std::vector<OValue *> deps;
    if (deps_value == nullptr) {
        return deps;
    }
    if (deps_value->type != JsonValue::Type::Array) {
        throw std::runtime_error("nix deps must be an array");
    }
    for (const auto &entry : deps_value->array_value) {
        std::string dep_json = json_dump(entry);
        OValue *dep = oval_from_json(dep_json.c_str());
        if (dep == nullptr) {
            throw std::runtime_error("failed to decode nix dep from JSON");
        }
        deps.push_back(dep);
    }
    return deps;
}

std::string extract_type_name(const JsonValue &json) {
    const JsonValue *t = json.get("t");
    if (t == nullptr || t->type != JsonValue::Type::String) {
        return "unknown";
    }
    return t->string_value;
}

std::vector<std::string> parse_outputs_from_show(const std::string &text, const std::string &drv_path) {
    JsonValue root = JsonParser(text).parse();
    const JsonValue *drv = root.get(drv_path);
    if (drv == nullptr || drv->type != JsonValue::Type::Object) {
        throw std::runtime_error("nix derivation show JSON missing derivation entry");
    }
    const JsonValue *outputs = drv->get("outputs");
    if (outputs == nullptr || outputs->type != JsonValue::Type::Object) {
        throw std::runtime_error("nix derivation show JSON missing outputs");
    }
    std::vector<std::string> result;
    for (const auto &entry : outputs->object_value) {
        result.push_back(entry.first);
    }
    return result;
}

std::string trim_copy(const std::string &text) {
    std::size_t start = 0;
    while (start < text.size() && std::isspace(static_cast<unsigned char>(text[start]))) {
        ++start;
    }
    std::size_t end = text.size();
    while (end > start && std::isspace(static_cast<unsigned char>(text[end - 1]))) {
        --end;
    }
    return text.substr(start, end - start);
}

} // namespace

OValue *instantiate_nix(OValue *source) {
    JsonValue json = value_to_json(source);
    if (extract_type_name(json) != "nix_expr") {
        throw std::runtime_error(
            "instantiate() expected a NixExpr (nix_expr^(...)_nix_expr block), got " +
            extract_type_name(json));
    }

    const JsonValue *body_value = json.get("body");
    if (body_value == nullptr || body_value->type != JsonValue::Type::String) {
        throw std::runtime_error("instantiate() nix_expr missing body");
    }
    std::vector<OValue *> deps = extract_deps(json.get("deps"));
    const std::string wrapper = "(let v = (" + body_value->string_value + "); in v.drvPath)";

    CommandResult eval = run_command({
        "nix", "--extra-experimental-features", "nix-command",
        "eval", "--raw", "--impure", "--expr", wrapper,
    });
    if (eval.exit_code != 0) {
        throw std::runtime_error(
            "nix eval failed while instantiating (exit " + std::to_string(eval.exit_code) + "):\nSTDERR:\n" +
            eval.err);
    }

    const std::string drv_path = trim_copy(eval.out);
    if (drv_path.rfind("/nix/store/", 0) != 0 || drv_path.size() < 4 ||
        drv_path.substr(drv_path.size() - 4) != ".drv") {
        throw std::runtime_error(
            "instantiate() expected the Nix expression to evaluate to a derivation "
            "(its .drvPath should be a /nix/store/*.drv path), got: \"" + drv_path + "\"");
    }

    CommandResult show = run_command({
        "nix", "--extra-experimental-features", "nix-command",
        "derivation", "show", drv_path,
    });
    if (show.exit_code != 0) {
        throw std::runtime_error(
            "nix derivation show failed for " + drv_path + " (exit " +
            std::to_string(show.exit_code) + "):\nSTDERR:\n" + show.err);
    }

    std::vector<std::string> outputs = parse_outputs_from_show(show.out, drv_path);
    std::vector<const char *> output_ptrs;
    output_ptrs.reserve(outputs.size());
    for (const auto &output : outputs) {
        output_ptrs.push_back(output.c_str());
    }

    return oval_derivation(
        drv_path.c_str(),
        output_ptrs.empty() ? nullptr : output_ptrs.data(),
        output_ptrs.size(),
        deps.empty() ? nullptr : deps.data(),
        deps.size());
}

OValue *realise_nix(OValue *source) {
    JsonValue json = value_to_json(source);
    if (extract_type_name(json) != "derivation") {
        throw std::runtime_error(
            "realise() expected a Derivation (the output of instantiate()), got " +
            extract_type_name(json));
    }

    const JsonValue *drv_path_value = json.get("drv_path");
    if (drv_path_value == nullptr || drv_path_value->type != JsonValue::Type::String) {
        throw std::runtime_error("realise() derivation missing drv_path");
    }
    const std::string drv_path = drv_path_value->string_value;
    const std::string target = drv_path + "^out";

    CommandResult build = run_command({
        "nix", "--extra-experimental-features", "nix-command",
        "build", target, "--no-link", "--print-out-paths",
    });
    if (build.exit_code != 0) {
        throw std::runtime_error(
            "nix build failed while realising " + drv_path + " (exit " +
            std::to_string(build.exit_code) + "):\nSTDERR:\n" + build.err);
    }

    std::string path;
    std::size_t start = 0;
    while (start < build.out.size()) {
        std::size_t end = build.out.find('\n', start);
        if (end == std::string::npos) {
            end = build.out.size();
        }
        std::string line = trim_copy(build.out.substr(start, end - start));
        if (!line.empty()) {
            path = line;
        }
        start = end + 1;
    }
    if (path.empty()) {
        throw std::runtime_error("nix build returned no output paths");
    }
    if (path.rfind("/nix/store/", 0) != 0) {
        throw std::runtime_error(
            "realise() expected nix build to print a /nix/store/* path, got: \"" + path + "\"");
    }

    return oval_store_path(path.c_str());
}

} // namespace olang
