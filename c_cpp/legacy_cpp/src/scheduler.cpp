#include "scheduler.hpp"

#include "nix_ops.hpp"
#include "nixos_ops.hpp"

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <map>
#include <mutex>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <vector>

namespace olang {
OValue *instantiate_nix(OValue *source);
OValue *realise_nix(OValue *source);
OValue *activate_nix(OValue *source, const std::string &profile, bool dry_run);

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

JsonValue value_to_json(OValue *value) {
    char *json = oval_to_json(value);
    if (json == nullptr) {
        throw std::runtime_error("failed to serialize OValue");
    }
    std::string text(json);
    std::free(json);
    return JsonParser(text).parse();
}

OValue *clone_value(OValue *value) {
    if (value == nullptr) {
        return nullptr;
    }
    char *json = oval_to_json(value);
    if (json == nullptr) {
        throw std::runtime_error("failed to serialize OValue for clone");
    }
    OValue *copy = oval_from_json(json);
    std::free(json);
    if (copy == nullptr) {
        throw std::runtime_error("failed to deserialize OValue clone");
    }
    return copy;
}

std::string type_name_from_json(const JsonValue &value) {
    const JsonValue *t = value.get("t");
    return (t != nullptr && t->type == JsonValue::Type::String) ? t->string_value : "unknown";
}

enum class RequestDispatchKind { Instantiate, Realise, Eval, Activate };

struct ParsedRequest {
    std::string fingerprint;
    JsonValue source_json;
    RequestDispatchKind kind;
    std::string profile;
    bool dry_run = false;
};

ParsedRequest parse_request(OValue *value) {
    JsonValue root = value_to_json(value);
    if (type_name_from_json(root) != "request") {
        throw std::runtime_error("expected request, got " + type_name_from_json(root));
    }

    const JsonValue *fingerprint = root.get("fingerprint");
    const JsonValue *source = root.get("source");
    const JsonValue *kind = root.get("kind");
    if (fingerprint == nullptr || fingerprint->type != JsonValue::Type::String ||
        source == nullptr || kind == nullptr) {
        throw std::runtime_error("request JSON missing required fields");
    }

    ParsedRequest result;
    result.fingerprint = fingerprint->string_value;
    result.source_json = *source;

    if (kind->type == JsonValue::Type::String) {
        if (kind->string_value == "instantiate") {
            result.kind = RequestDispatchKind::Instantiate;
        } else if (kind->string_value == "realise") {
            result.kind = RequestDispatchKind::Realise;
        } else if (kind->string_value == "eval") {
            result.kind = RequestDispatchKind::Eval;
        } else {
            throw std::runtime_error("unknown request kind: " + kind->string_value);
        }
        return result;
    }

    if (kind->type != JsonValue::Type::Object || kind->object_value.size() != 1) {
        throw std::runtime_error("unsupported structured request kind");
    }
    const auto &entry = *kind->object_value.begin();
    if (entry.first == "activate") {
        result.kind = RequestDispatchKind::Activate;
        const JsonValue *profile = entry.second.get("profile");
        const JsonValue *dry_run = entry.second.get("dry_run");
        if (profile == nullptr || profile->type != JsonValue::Type::String ||
            dry_run == nullptr || dry_run->type != JsonValue::Type::Bool) {
            throw std::runtime_error("activate request missing profile/dry_run");
        }
        result.profile = profile->string_value;
        result.dry_run = dry_run->bool_value;
        return result;
    }
    if (entry.first == "eval") {
        result.kind = RequestDispatchKind::Eval;
        return result;
    }
    throw std::runtime_error("unknown structured request kind: " + entry.first);
}

std::map<std::string, std::vector<std::string>> build_dep_graph(
    const std::map<std::string, OValue *> &all) {
    std::map<std::string, std::vector<std::string>> graph;
    for (const auto &entry : all) {
        ParsedRequest req = parse_request(entry.second);
        std::vector<std::string> deps;
        if (type_name_from_json(req.source_json) == "request") {
            const JsonValue *dep_fp = req.source_json.get("fingerprint");
            if (dep_fp != nullptr && dep_fp->type == JsonValue::Type::String && all.count(dep_fp->string_value) != 0) {
                deps.push_back(dep_fp->string_value);
            }
        }
        graph.emplace(entry.first, std::move(deps));
    }
    return graph;
}

OValue *resolve_source(OValue *req, const std::map<std::string, OValue *> &resolved) {
    ParsedRequest parsed = parse_request(req);
    if (type_name_from_json(parsed.source_json) != "request") {
        std::string source_json = json_dump(parsed.source_json);
        OValue *source = oval_from_json(source_json.c_str());
        if (source == nullptr) {
            throw std::runtime_error("failed to deserialize request source");
        }
        return source;
    }
    const JsonValue *fingerprint = parsed.source_json.get("fingerprint");
    if (fingerprint == nullptr || fingerprint->type != JsonValue::Type::String) {
        throw std::runtime_error("request source missing dependency fingerprint");
    }
    auto it = resolved.find(fingerprint->string_value);
    if (it == resolved.end()) {
        throw std::runtime_error("scheduler: dependency not yet resolved");
    }
    return clone_value(it->second);
}

void free_request_map(std::map<std::string, OValue *> &values) {
    for (auto &entry : values) {
        if (entry.second != nullptr) {
            oval_release(entry.second);
        }
    }
    values.clear();
}

} // namespace

DiskCache::DiskCache(const std::string &dir) : dir_(dir) {
    std::filesystem::create_directories(dir_);
}

std::string DiskCache::default_dir() {
    const char *xdg = std::getenv("XDG_CACHE_HOME");
    if (xdg != nullptr && *xdg != '\0') {
        return (std::filesystem::path(xdg) / "o-lang" / "sched").string();
    }
    const char *home = std::getenv("HOME");
    if (home != nullptr && *home != '\0') {
        return (std::filesystem::path(home) / ".cache" / "o-lang" / "sched").string();
    }
    return "/tmp/o-lang-cache/sched";
}

OValue *DiskCache::get(const std::string &fingerprint) {
    try {
        const std::filesystem::path path = std::filesystem::path(dir_) / (fingerprint + ".json");
        std::ifstream in(path, std::ios::binary);
        if (!in) {
            return nullptr;
        }
        std::string json((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
        if (json.empty()) {
            return nullptr;
        }
        return oval_from_json(json.c_str());
    } catch (...) {
        return nullptr;
    }
}

void DiskCache::put(const std::string &fingerprint, OValue *value) {
    try {
        char *json = oval_to_json(value);
        if (json == nullptr) {
            std::cerr << "[o-lang scheduler] cache serialize failed for " << fingerprint.substr(0, 8) << "\n";
            return;
        }
        const std::filesystem::path path = std::filesystem::path(dir_) / (fingerprint + ".json");
        const std::filesystem::path tmp = std::filesystem::path(dir_) / (fingerprint + ".json.tmp");
        {
            std::ofstream out(tmp, std::ios::binary | std::ios::trunc);
            if (!out) {
                std::free(json);
                std::cerr << "[o-lang scheduler] cache write failed for " << fingerprint.substr(0, 8) << "\n";
                return;
            }
            out << json;
            if (!out.good()) {
                std::free(json);
                std::cerr << "[o-lang scheduler] cache write failed for " << fingerprint.substr(0, 8) << "\n";
                return;
            }
        }
        std::free(json);
        std::filesystem::rename(tmp, path);
    } catch (const std::exception &e) {
        std::cerr << "[o-lang scheduler] cache error for " << fingerprint.substr(0, 8) << ": " << e.what() << "\n";
    }
}

AutonomousScheduler::AutonomousScheduler()
    : parallelism_(std::thread::hardware_concurrency() == 0
                        ? 4
                        : std::min<std::size_t>(std::thread::hardware_concurrency(), 8)) {
    try {
        disk_cache_ = std::make_unique<DiskCache>(DiskCache::default_dir());
    } catch (...) {
        disk_cache_.reset();
    }
}

AutonomousScheduler::AutonomousScheduler(const std::string &cache_dir)
    : parallelism_(std::thread::hardware_concurrency() == 0
                        ? 4
                        : std::min<std::size_t>(std::thread::hardware_concurrency(), 8)) {
    try {
        disk_cache_ = std::make_unique<DiskCache>(cache_dir);
    } catch (...) {
        disk_cache_.reset();
    }
}

void AutonomousScheduler::set_parallelism(size_t n) {
    parallelism_ = std::max<std::size_t>(1, n);
}

OValue *AutonomousScheduler::cache_get(const std::string &fingerprint) {
    auto it = mem_cache.find(fingerprint);
    if (it != mem_cache.end() && it->second != nullptr) {
        return clone_value(it->second);
    }
    if (disk_cache_) {
        OValue *hit = disk_cache_->get(fingerprint);
        if (hit != nullptr) {
            auto existing = mem_cache.find(fingerprint);
            if (existing != mem_cache.end() && existing->second != nullptr) {
                oval_release(existing->second);
            }
            mem_cache[fingerprint] = clone_value(hit);
            return hit;
        }
    }
    return nullptr;
}

void AutonomousScheduler::cache_put(const std::string &fingerprint, OValue *value) {
    if (disk_cache_) {
        disk_cache_->put(fingerprint, value);
    }
    auto it = mem_cache.find(fingerprint);
    if (it != mem_cache.end() && it->second != nullptr) {
        oval_release(it->second);
    }
    mem_cache[fingerprint] = clone_value(value);
}

std::map<std::string, OValue *> AutonomousScheduler::execute_batch(
    const std::vector<OValue *> &roots,
    std::function<OValue *(OValue *)> eval_fn) {
    std::map<std::string, OValue *> all;
    for (OValue *root : roots) {
        collect_transitive_requests(root, all);
    }
    if (all.empty()) {
        return {};
    }

    std::map<std::string, OValue *> resolved;
    for (const auto &entry : all) {
        OValue *hit = cache_get(entry.first);
        if (hit != nullptr) {
            resolved.emplace(entry.first, hit);
        }
    }

    const auto dep_graph = build_dep_graph(all);
    std::set<std::string> pending;
    for (const auto &entry : all) {
        if (resolved.find(entry.first) == resolved.end()) {
            pending.insert(entry.first);
        }
    }

    try {
        while (!pending.empty()) {
            std::vector<std::string> ready;
            for (const auto &fp : pending) {
                auto it = dep_graph.find(fp);
                bool ok = true;
                if (it != dep_graph.end()) {
                    for (const auto &dep : it->second) {
                        if (resolved.find(dep) == resolved.end()) {
                            ok = false;
                            break;
                        }
                    }
                }
                if (ok) {
                    ready.push_back(fp);
                }
            }

            if (ready.empty()) {
                throw std::runtime_error("autonomous scheduler: dependency stall (possible cycle)");
            }

            std::vector<std::string> threadable;
            std::vector<std::string> serial;
            for (const auto &fp : ready) {
                ParsedRequest req = parse_request(all.at(fp));
                if (req.kind == RequestDispatchKind::Instantiate ||
                    req.kind == RequestDispatchKind::Realise ||
                    req.kind == RequestDispatchKind::Activate) {
                    threadable.push_back(fp);
                } else {
                    serial.push_back(fp);
                }
            }

            std::vector<std::string> wave;
            for (std::size_t i = 0; i < threadable.size() && i < parallelism_; ++i) {
                wave.push_back(threadable[i]);
            }

            struct ThreadResult {
                std::string fp;
                OValue *value = nullptr;
                std::string error;
            };
            std::vector<ThreadResult> thread_results(wave.size());
            std::vector<std::thread> threads;
            threads.reserve(wave.size());

            for (std::size_t i = 0; i < wave.size(); ++i) {
                const std::string fp = wave[i];
                ParsedRequest parsed = parse_request(all.at(fp));
                OValue *source = resolve_source(all.at(fp), resolved);
                thread_results[i].fp = fp;
                threads.emplace_back([parsed, source, &slot = thread_results[i]]() {
                    try {
                        if (parsed.kind == RequestDispatchKind::Instantiate) {
                            slot.value = instantiate_nix(source);
                        } else if (parsed.kind == RequestDispatchKind::Realise) {
                            slot.value = realise_nix(source);
                        } else if (parsed.kind == RequestDispatchKind::Activate) {
                            slot.value = activate_nix(source, parsed.profile, parsed.dry_run);
                        } else {
                            throw std::runtime_error("unexpected non-threadable request kind");
                        }
                    } catch (const std::exception &e) {
                        slot.error = e.what();
                    }
                    if (source != nullptr) {
                        oval_release(source);
                    }
                });
            }
            for (auto &thread : threads) {
                thread.join();
            }
            for (auto &result : thread_results) {
                if (!result.error.empty()) {
                    throw std::runtime_error(
                        "autonomous scheduler: request " + result.fp.substr(0, std::min<std::size_t>(8, result.fp.size())) +
                        " failed: " + result.error);
                }
                cache_put(result.fp, result.value);
                resolved[result.fp] = result.value;
                pending.erase(result.fp);
            }

            if (!serial.empty()) {
                const std::string fp = serial.front();
                if (!eval_fn) {
                    throw std::runtime_error(
                        "autonomous scheduler: encountered RequestKind::Eval but no eval_fn callback was provided");
                }
                OValue *result = eval_fn(all.at(fp));
                if (result == nullptr) {
                    throw std::runtime_error("autonomous scheduler: eval_fn returned null");
                }
                auto it = mem_cache.find(fp);
                if (it != mem_cache.end() && it->second != nullptr) {
                    oval_release(it->second);
                }
                mem_cache[fp] = clone_value(result);
                resolved[fp] = result;
                pending.erase(fp);
            }
        }
    } catch (...) {
        free_request_map(all);
        throw;
    }

    free_request_map(all);
    return resolved;
}

OValue *AutonomousScheduler::execute(OValue *req) {
    ParsedRequest parsed = parse_request(req);
    OValue *hit = cache_get(parsed.fingerprint);
    if (hit != nullptr) {
        return hit;
    }
    auto results = execute_batch({req}, nullptr);
    auto it = results.find(parsed.fingerprint);
    if (it == results.end()) {
        throw std::runtime_error("scheduler: root request not in results");
    }
    return it->second;
}

void collect_transitive_requests(OValue *req, std::map<std::string, OValue *> &out) {
    ParsedRequest parsed = parse_request(req);
    if (out.find(parsed.fingerprint) != out.end()) {
        return;
    }
    out[parsed.fingerprint] = clone_value(req);
    if (type_name_from_json(parsed.source_json) == "request") {
        std::string nested_json = json_dump(parsed.source_json);
        OValue *nested = oval_from_json(nested_json.c_str());
        if (nested == nullptr) {
            throw std::runtime_error("failed to deserialize nested request");
        }
        collect_transitive_requests(nested, out);
        oval_release(nested);
    }
}

} // namespace olang
