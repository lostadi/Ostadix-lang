#include "eval.hpp"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdlib>
#include <filesystem>
#include <limits>
#include <set>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace olang {
OValue *instantiate_nix(OValue *source);
OValue *realise_nix(OValue *source);
OValue *activate_nix(OValue *source, const std::string &profile, bool dry_run);
}

namespace {

constexpr const char *kDefaultSystemProfile = "/nix/var/nix/profiles/system";
constexpr std::array<const char *, 8> kPureBackends = {
    "nix", "nix_expr", "nix_store", "nixos_test",
    "html", "markdown", "latex", "text"
};

std::string to_owned(char *s) {
    std::string out = s == nullptr ? std::string() : std::string(s);
    std::free(s);
    return out;
}

std::string maybe(const char *s) {
    return s == nullptr ? std::string() : std::string(s);
}

std::string short_fp(const std::string &fp) {
    return fp.substr(0, std::min<std::size_t>(8, fp.size()));
}

bool whitespace_only(const char *text) {
    if (text == nullptr || *text == '\0') {
        return false;
    }
    for (const unsigned char *p = reinterpret_cast<const unsigned char *>(text); *p != 0; ++p) {
        if (!std::isspace(*p)) {
            return false;
        }
    }
    return true;
}

std::string trim_copy(const std::string &s) {
    const auto first = s.find_first_not_of(" \t\r\n");
    if (first == std::string::npos) {
        return {};
    }
    const auto last = s.find_last_not_of(" \t\r\n");
    return s.substr(first, last - first + 1U);
}

struct BlockOptions {
    bool lazy = false;
    bool defer = false;
    bool has_capability_binding = false;
};

bool is_backend_authority_attr(const std::string &entry) {
    return entry == "fs_read" || entry == "fs_write" ||
           entry == "network" || entry == "process";
}

BlockOptions parse_block_options(const char *attr, const std::string &lang) {
    BlockOptions options;
    if (attr == nullptr) {
        return options;
    }

    std::set<std::string> seen;
    const std::string attrs = attr;
    std::size_t start = 0U;
    while (start <= attrs.size()) {
        const std::size_t comma = attrs.find(',', start);
        const std::size_t end = comma == std::string::npos ? attrs.size() : comma;
        const std::string entry = trim_copy(attrs.substr(start, end - start));
        if (!seen.insert(entry).second) {
            throw std::runtime_error("duplicate block attribute `{" + entry + "}` on " + lang + "^");
        }

        if (entry == "lazy") {
            if (options.defer) {
                throw std::runtime_error("a block cannot combine `lazy` and `defer`");
            }
            options.lazy = true;
        } else if (entry == "defer") {
            if (options.lazy) {
                throw std::runtime_error("a block cannot combine `lazy` and `defer`");
            }
            options.defer = true;
        } else if (entry.rfind("cap=", 0) == 0) {
            const std::string name = entry.substr(4);
            if (name.empty() || options.has_capability_binding) {
                throw std::runtime_error("a block must name exactly one backend capability binding");
            }
            options.has_capability_binding = true;
        } else if (!is_backend_authority_attr(entry)) {
            throw std::runtime_error(
                "Unknown block attribute `{" + entry + "}` on " + lang +
                "^. Known attributes: lazy, defer, cap=name, fs_read, fs_write, network, process.");
        }

        if (comma == std::string::npos) {
            break;
        }
        start = comma + 1U;
    }
    return options;
}

std::string json_quote(const std::string &s) {
    std::string out;
    out.reserve(s.size() + 2);
    out.push_back('"');
    for (unsigned char ch : s) {
        switch (ch) {
        case '"': out += "\\\""; break;
        case '\\': out += "\\\\"; break;
        case '\b': out += "\\b"; break;
        case '\f': out += "\\f"; break;
        case '\n': out += "\\n"; break;
        case '\r': out += "\\r"; break;
        case '\t': out += "\\t"; break;
        default:
            if (ch < 0x20U) {
                static const char hex[] = "0123456789abcdef";
                out += "\\u00";
                out.push_back(hex[(ch >> 4U) & 0x0FU]);
                out.push_back(hex[ch & 0x0FU]);
            } else {
                out.push_back(static_cast<char>(ch));
            }
        }
    }
    out.push_back('"');
    return out;
}

std::string format_float(double v) {
    if (std::isnan(v)) {
        return "nan";
    }
    if (std::isinf(v)) {
        return v < 0 ? "-inf" : "inf";
    }
    std::string s = std::to_string(v);
    if (s.find('.') != std::string::npos) {
        while (!s.empty() && s.back() == '0') {
            s.pop_back();
        }
        if (!s.empty() && s.back() == '.') {
            s.push_back('0');
        }
    }
    return s;
}

std::vector<std::pair<std::string, OValue *>> map_entries(const OValue *map) {
    std::vector<std::pair<std::string, OValue *>> out;
    if (map == nullptr || map->tag != OVAL_MAP || map->data.map == nullptr) {
        return out;
    }
    OValueMap *m = map->data.map;
    for (std::size_t i = 0; i < m->bucket_count; ++i) {
        for (OMapEntry *entry = m->buckets[i]; entry != nullptr; entry = entry->next) {
            out.emplace_back(maybe(entry->key), entry->value);
        }
    }
    return out;
}

OValueMap *scope_to_bindings(const std::map<std::string, OValue *> &scope) {
    OValue *wrapper = oval_map();
    if (wrapper == nullptr || wrapper->tag != OVAL_MAP) {
        throw std::runtime_error("failed to allocate bindings map");
    }
    for (const auto &entry : scope) {
        oval_map_set(wrapper, entry.first.c_str(), entry.second);
    }
    return wrapper->data.map;
}

RequestKind make_request_kind(RequestKindTag tag) {
    RequestKind kind{};
    kind.tag = tag;
    kind.lang = nullptr;
    kind.env_id = 0U;
    kind.cacheable = false;
    kind.profile = nullptr;
    kind.dry_run = false;
    return kind;
}

RequestKind make_eval_kind(const std::string &lang, uint32_t env_id, bool cacheable) {
    RequestKind kind = make_request_kind(REQ_EVAL);
    kind.lang = const_cast<char *>(lang.c_str());
    kind.env_id = env_id;
    kind.cacheable = cacheable;
    return kind;
}

RequestKind make_activate_kind(const std::string &profile, bool dry_run) {
    RequestKind kind = make_request_kind(REQ_ACTIVATE);
    kind.profile = const_cast<char *>(profile.c_str());
    kind.dry_run = dry_run;
    return kind;
}

const char *type_name(const OValue *v) { return oval_type_name(v); }

std::string splice_repr(const OValue *v) {
    return to_owned(oval_splice_repr(v));
}

std::string html_escape(const std::string &s) {
    std::string out;
    out.reserve(s.size());
    for (char ch : s) {
        switch (ch) {
        case '&': out += "&amp;"; break;
        case '<': out += "&lt;"; break;
        case '>': out += "&gt;"; break;
        case '"': out += "&quot;"; break;
        default: out.push_back(ch); break;
        }
    }
    return out;
}

std::string render_html_blob(const std::string &b64, const std::string &mime) {
    if (mime.rfind("image/", 0) == 0) {
        return "<img src=\"data:" + mime + ";base64," + b64 + "\" />";
    }
    if (mime == "text/html") {
        std::size_t len = 0;
        unsigned char *decoded = base64_decode(b64.c_str(), &len);
        if (decoded != nullptr) {
            std::string text(reinterpret_cast<char *>(decoded), len);
            std::free(decoded);
            return text;
        }
        return "<!-- blob decode error: " + mime + " -->";
    }
    if (mime.rfind("text/", 0) == 0) {
        std::size_t len = 0;
        unsigned char *decoded = base64_decode(b64.c_str(), &len);
        if (decoded != nullptr) {
            std::string text(reinterpret_cast<char *>(decoded), len);
            std::free(decoded);
            return html_escape(text);
        }
    }
    return "<a href=\"data:" + mime + ";base64," + b64 + "\">[blob " + mime + ", " + std::to_string((b64.size() * 3U) / 4U) + " bytes (base64)]</a>";
}

std::string render_nix(const OValue *val) {
    switch (val->tag) {
    case OVAL_NULL:
        return "null";
    case OVAL_BOOL:
        return val->data.bool_val ? "true" : "false";
    case OVAL_INT:
        return std::to_string(val->data.int_val);
    case OVAL_FLOAT:
        return format_float(val->data.float_val);
    case OVAL_STR:
    case OVAL_HTML:
    case OVAL_STORE_PATH:
    case OVAL_EXPR:
    case OVAL_SYSTEM:
        return json_quote(maybe(val->data.str_val));
    case OVAL_LIST: {
        std::string items;
        for (std::size_t i = 0; i < val->data.list.len; ++i) {
            if (i != 0U) items += ' ';
            items += render_nix(val->data.list.items[i]);
        }
        return "[ " + items + " ]";
    }
    case OVAL_MAP: {
        std::string items;
        const auto entries = map_entries(val);
        for (std::size_t i = 0; i < entries.size(); ++i) {
            if (i != 0U) items += ' ';
            items += entries[i].first + " = " + render_nix(entries[i].second) + ";";
        }
        return "{ " + items + " }";
    }
    case OVAL_BLOB:
        return json_quote(maybe(val->data.blob.data));
    case OVAL_NIX_EXPR:
        return "(" + maybe(val->data.nix_expr.body) + ")";
    case OVAL_DERIVATION:
        return json_quote(maybe(val->data.derivation.drv_path));
    case OVAL_REQUEST:
        return json_quote("<request fp=" + short_fp(maybe(val->data.request.fingerprint)) + ">");
    case OVAL_THUNK:
        return "(" + maybe(val->data.nix_expr.body) + ")";
    }
    return "null";
}

std::string render_python(const OValue *val) {
    switch (val->tag) {
    case OVAL_NULL:
        return "None";
    case OVAL_BOOL:
        return val->data.bool_val ? "True" : "False";
    case OVAL_INT:
        return std::to_string(val->data.int_val);
    case OVAL_FLOAT: {
        std::string s = format_float(val->data.float_val);
        if (s.find('.') == std::string::npos && s.find('e') == std::string::npos && s.find('E') == std::string::npos) {
            s += ".0";
        }
        return s;
    }
    case OVAL_STR:
        return json_quote(maybe(val->data.str_val));
    case OVAL_HTML:
        return "OHtml(" + json_quote(maybe(val->data.str_val)) + ")";
    case OVAL_STORE_PATH:
        return "OStorePath(" + json_quote(maybe(val->data.str_val)) + ")";
    case OVAL_EXPR:
        return "OExprValue(" + json_quote(maybe(val->data.str_val)) + ")";
    case OVAL_SYSTEM:
        return "OSystem(" + json_quote(maybe(val->data.str_val)) + ")";
    case OVAL_LIST: {
        std::string items;
        for (std::size_t i = 0; i < val->data.list.len; ++i) {
            if (i != 0U) items += ", ";
            items += render_python(val->data.list.items[i]);
        }
        return "[" + items + "]";
    }
    case OVAL_MAP: {
        std::string items;
        const auto entries = map_entries(val);
        for (std::size_t i = 0; i < entries.size(); ++i) {
            if (i != 0U) items += ", ";
            items += json_quote(entries[i].first) + ": " + render_python(entries[i].second);
        }
        return "{" + items + "}";
    }
    case OVAL_BLOB:
        return "{'mime': " + json_quote(maybe(val->data.blob.mime)) + ", 'base64': " + json_quote(maybe(val->data.blob.data)) + "}";
    case OVAL_NIX_EXPR: {
        std::string deps;
        for (std::size_t i = 0; i < val->data.nix_expr.deps_len; ++i) {
            if (i != 0U) deps += ", ";
            deps += render_python(val->data.nix_expr.deps[i]);
        }
        return "ONixExpr(" + json_quote(maybe(val->data.nix_expr.body)) + ", fp=" + json_quote(maybe(val->data.nix_expr.fingerprint)) + ", deps=[" + deps + "])";
    }
    case OVAL_DERIVATION: {
        std::string outs;
        for (std::size_t i = 0; i < val->data.derivation.outputs_len; ++i) {
            if (i != 0U) outs += ", ";
            outs += json_quote(maybe(val->data.derivation.outputs[i]));
        }
        return "ODerivation(" + json_quote(maybe(val->data.derivation.drv_path)) + ", outputs=[" + outs + "])";
    }
    case OVAL_REQUEST:
        return "ORequest(fp=" + json_quote(maybe(val->data.request.fingerprint)) + ")";
    case OVAL_THUNK: {
        std::string deps;
        for (std::size_t i = 0; i < val->data.nix_expr.deps_len; ++i) {
            if (i != 0U) deps += ", ";
            deps += render_python(val->data.nix_expr.deps[i]);
        }
        return "OThunk(" + json_quote(maybe(val->data.nix_expr.body)) + ", fp=" + json_quote(maybe(val->data.nix_expr.fingerprint)) + ", deps=[" + deps + "])";
    }
    }
    return "None";
}

std::string render_html(const OValue *val) {
    switch (val->tag) {
    case OVAL_NULL:
        return {};
    case OVAL_BOOL:
        return html_escape(val->data.bool_val ? "true" : "false");
    case OVAL_INT:
        return html_escape(std::to_string(val->data.int_val));
    case OVAL_FLOAT:
        return html_escape(format_float(val->data.float_val));
    case OVAL_STR:
        // Untrusted text — escape. Trusted raw HTML must be OVAL_HTML.
        return html_escape(maybe(val->data.str_val));
    case OVAL_HTML:
        return maybe(val->data.str_val);
    case OVAL_STORE_PATH:
        return "<code class=\"o-store-path\">" + html_escape(maybe(val->data.str_val)) + "</code>";
    case OVAL_EXPR:
        return "<code class=\"o-expr\">" + html_escape(maybe(val->data.str_val)) + "</code>";
    case OVAL_SYSTEM:
        return "<code class=\"o-system\">" + html_escape(maybe(val->data.str_val)) + "</code>";
    case OVAL_LIST: {
        std::string items;
        for (std::size_t i = 0; i < val->data.list.len; ++i) {
            items += "<li>" + render_html(val->data.list.items[i]) + "</li>";
        }
        return "<ul>" + items + "</ul>";
    }
    case OVAL_MAP: {
        std::string out;
        for (const auto &entry : map_entries(val)) {
            out += "<div data-o-key=\"" + html_escape(entry.first) + "\">" + render_html(entry.second) + "</div>";
        }
        return out;
    }
    case OVAL_BLOB:
        return render_html_blob(maybe(val->data.blob.data), maybe(val->data.blob.mime));
    case OVAL_NIX_EXPR:
        return "<code class=\"o-nix-expr\" data-fp=\"" + html_escape(maybe(val->data.nix_expr.fingerprint)) + "\">" + html_escape(maybe(val->data.nix_expr.body)) + "</code>";
    case OVAL_DERIVATION: {
        std::string outs;
        for (std::size_t i = 0; i < val->data.derivation.outputs_len; ++i) {
            if (i != 0U) outs += ',';
            outs += maybe(val->data.derivation.outputs[i]);
        }
        return "<code class=\"o-derivation\" data-outputs=\"" + html_escape(outs) + "\">" + html_escape(maybe(val->data.derivation.drv_path)) + "</code>";
    }
    case OVAL_REQUEST:
        return "<code class=\"o-request\" data-fp=\"" + html_escape(short_fp(maybe(val->data.request.fingerprint))) + "\">&lt;request&gt;</code>";
    case OVAL_THUNK:
        return "<code class=\"o-thunk\" data-fp=\"" + html_escape(short_fp(maybe(val->data.nix_expr.fingerprint))) + "\">" + html_escape(maybe(val->data.nix_expr.body)) + "</code>";
    }
    return {};
}

std::string render_latex(const OValue *val) {
    auto tt = [](const std::string &s) {
        std::string escaped = s;
        std::size_t pos = 0;
        while ((pos = escaped.find('_', pos)) != std::string::npos) {
            escaped.replace(pos, 1, "\\_");
            pos += 2;
        }
        return "\\texttt{" + escaped + "}";
    };
    switch (val->tag) {
    case OVAL_NULL: return {};
    case OVAL_BOOL: return val->data.bool_val ? "true" : "false";
    case OVAL_INT: return std::to_string(val->data.int_val);
    case OVAL_FLOAT: return format_float(val->data.float_val);
    case OVAL_STR:
    case OVAL_HTML: return maybe(val->data.str_val);
    case OVAL_STORE_PATH:
    case OVAL_EXPR:
    case OVAL_SYSTEM: return tt(maybe(val->data.str_val));
    case OVAL_LIST: {
        std::string out;
        for (std::size_t i = 0; i < val->data.list.len; ++i) {
            if (i != 0U) out += ", ";
            out += render_latex(val->data.list.items[i]);
        }
        return out;
    }
    case OVAL_MAP: {
        std::string out;
        const auto entries = map_entries(val);
        for (std::size_t i = 0; i < entries.size(); ++i) {
            if (i != 0U) out += ", ";
            out += entries[i].first + ": " + render_latex(entries[i].second);
        }
        return out;
    }
    case OVAL_BLOB:
        return tt("<blob:" + maybe(val->data.blob.mime) + ">");
    case OVAL_NIX_EXPR:
        return tt(maybe(val->data.nix_expr.body));
    case OVAL_DERIVATION:
        return tt(maybe(val->data.derivation.drv_path));
    case OVAL_REQUEST:
        return tt("<request fp=" + short_fp(maybe(val->data.request.fingerprint)) + ">");
    case OVAL_THUNK:
        return tt(maybe(val->data.nix_expr.body));
    }
    return {};
}

std::string render_markdown(const OValue *val) {
    switch (val->tag) {
    case OVAL_NULL: return {};
    case OVAL_BOOL: return val->data.bool_val ? "true" : "false";
    case OVAL_INT: return std::to_string(val->data.int_val);
    case OVAL_FLOAT: return format_float(val->data.float_val);
    case OVAL_STR:
    case OVAL_HTML: return maybe(val->data.str_val);
    case OVAL_STORE_PATH:
    case OVAL_EXPR:
    case OVAL_SYSTEM: return "`" + maybe(val->data.str_val) + "`";
    case OVAL_LIST: {
        std::string out;
        for (std::size_t i = 0; i < val->data.list.len; ++i) {
            if (i != 0U) out += "\n";
            out += render_markdown(val->data.list.items[i]);
        }
        return out;
    }
    case OVAL_MAP: {
        std::string out;
        const auto entries = map_entries(val);
        for (std::size_t i = 0; i < entries.size(); ++i) {
            if (i != 0U) out += "\n";
            out += "**" + entries[i].first + "**: " + render_markdown(entries[i].second);
        }
        return out;
    }
    case OVAL_BLOB:
        return "<blob:" + maybe(val->data.blob.mime) + ">";
    case OVAL_NIX_EXPR:
        return "`" + maybe(val->data.nix_expr.body) + "`";
    case OVAL_DERIVATION:
        return "`" + maybe(val->data.derivation.drv_path) + "`";
    case OVAL_REQUEST:
        return "`<request fp=" + short_fp(maybe(val->data.request.fingerprint)) + ">`";
    case OVAL_THUNK:
        return "`" + maybe(val->data.nix_expr.body) + "`";
    }
    return {};
}

} // namespace

namespace olang {

void ImmediateExecutor::seed_cache(const std::string &fingerprint, OValue *value) {
    auto it = cache_.find(fingerprint);
    if (it != cache_.end()) {
        oval_release(it->second);
        it->second = oval_retain(value);
    } else {
        cache_.emplace(fingerprint, oval_retain(value));
    }
}

OValue *ImmediateExecutor::execute(OValue *req) {
    if (!oval_is_request(req)) {
        throw std::runtime_error(std::string("Executor::execute expected a Request, got ") + type_name(req));
    }

    const RequestKind &kind = req->data.request.kind;
    const std::string fingerprint = maybe(req->data.request.fingerprint);
    const bool consult_cache = !(kind.tag == REQ_EVAL && !kind.cacheable) && kind.tag != REQ_ACTIVATE;

    if (consult_cache) {
        auto hit = cache_.find(fingerprint);
        if (hit != cache_.end()) {
            return oval_retain(hit->second);
        }
    }

    OValue *resolved_source = req->data.request.source;
    if (oval_is_request(resolved_source)) {
        resolved_source = execute(resolved_source);
    } else {
        resolved_source = oval_retain(resolved_source);
    }

    OValue *result = nullptr;
    switch (kind.tag) {
    case REQ_INSTANTIATE:
        result = instantiate_nix(resolved_source);
        break;
    case REQ_REALISE:
        result = realise_nix(resolved_source);
        break;
    case REQ_EVAL:
        oval_release(resolved_source);
        throw std::runtime_error("ImmediateExecutor cannot perform RequestKind::Eval directly — the Evaluator dispatches Eval via force_request -> exec_eval");
    case REQ_ACTIVATE:
        result = activate_nix(resolved_source, maybe(kind.profile), kind.dry_run);
        break;
    }
    oval_release(resolved_source);

    if (consult_cache) {
        seed_cache(fingerprint, result);
    }
    return result;
}

Evaluator::Evaluator(const std::string &shim_dir)
    : registry_(), shim_dir_(shim_dir), registered_backends_(), policy_(Policy::Eager),
      executor_(std::make_unique<ImmediateExecutor>()), eval_cache_(), scheduler_(), autonomous_buffer_() {}

Evaluator::~Evaluator() {
    for (auto &entry : eval_cache_) {
        oval_release(entry.second);
    }
    for (OValue *value : autonomous_buffer_) {
        oval_release(value);
    }
    registry_.cleanup_all();
}

void Evaluator::set_registered_backends(const std::set<std::string> &backends) {
    registered_backends_ = backends;
}

void Evaluator::set_executor(std::unique_ptr<Executor> exec) {
    executor_ = std::move(exec);
}

bool Evaluator::is_pure_backend(const std::string &lang) {
    return std::find(kPureBackends.begin(), kPureBackends.end(), lang) != kPureBackends.end();
}

bool Evaluator::is_schedulable_request(OValue *v) {
    if (!oval_is_request(v)) {
        return false;
    }
    const RequestKind &kind = v->data.request.kind;
    if (kind.tag == REQ_EVAL) {
        return false;
    }
    if (kind.tag == REQ_ACTIVATE && !kind.dry_run) {
        return false;
    }
    return true;
}

OValue *Evaluator::auto_resolve(OValue *v) {
    if (!oval_is_request(v)) {
        return v;
    }
    if (policy_ == Policy::Eager) {
        OValue *out = force_request(v);
        oval_release(v);
        return out;
    }
    if (policy_ == Policy::Autonomous) {
        const RequestKind &kind = v->data.request.kind;
        if (kind.tag == REQ_EVAL || (kind.tag == REQ_ACTIVATE && !kind.dry_run)) {
            OValue *out = force_request(v);
            oval_release(v);
            return out;
        }
        autonomous_buffer_.push_back(oval_retain(v));
        return v;
    }
    return v;
}

OValue *Evaluator::force_request(OValue *req) {
    if (!oval_is_request(req)) {
        throw std::runtime_error(std::string("force_request expected a Request, got ") + type_name(req));
    }
    if (req->data.request.kind.tag == REQ_EVAL) {
        return exec_eval(req);
    }
    if (req->data.request.kind.tag == REQ_ACTIVATE && !req->data.request.kind.dry_run) {
        return executor_->execute(req);
    }
    if (policy_ == Policy::Autonomous) {
        return scheduler_.execute(req);
    }
    return executor_->execute(req);
}

void Evaluator::flush_autonomous_buffer() {
    std::vector<OValue *> buffer;
    buffer.swap(autonomous_buffer_);
    try {
        if (!buffer.empty()) {
            auto results = scheduler_.execute_batch(buffer, nullptr);
            for (auto &entry : results) {
                oval_release(entry.second);
            }
        }
    } catch (...) {
        for (OValue *v : buffer) {
            oval_release(v);
        }
        throw;
    }
    for (OValue *v : buffer) {
        oval_release(v);
    }
}

OValue *Evaluator::resolve_from_cache(OValue *v) {
    if (!oval_is_request(v)) {
        return nullptr;
    }
    const std::string fingerprint = maybe(v->data.request.fingerprint);
    if (v->data.request.kind.tag == REQ_EVAL) {
        auto it = eval_cache_.find(fingerprint);
        return it == eval_cache_.end() ? nullptr : oval_retain(it->second);
    }
    return scheduler_.cache_get(fingerprint);
}

OValue *Evaluator::resolve_for_splice(OValue *v) {
    if (oval_is_request(v) && v->data.request.kind.tag == REQ_EVAL) {
        if (v->data.request.kind.cacheable) {
            OValue *out = force_request(v);
            oval_release(v);
            return out;
        }
        const std::string lang = maybe(v->data.request.kind.lang);
        oval_release(v);
        throw std::runtime_error(
            "Cannot splice a {defer} thunk (`" + lang + "{defer}^...`) into source text — {defer} is non-cacheable and forcing it implicitly could re-run side effects unexpectedly. Wrap the splice in now(...) to force explicitly.");
    }
    return v;
}

OValue *Evaluator::exec_eval(OValue *req) {
    if (!oval_is_request(req) || req->data.request.kind.tag != REQ_EVAL) {
        throw std::runtime_error("exec_eval expected RequestKind::Eval");
    }
    const RequestKind &kind = req->data.request.kind;
    const std::string fingerprint = maybe(req->data.request.fingerprint);

    if (kind.cacheable) {
        auto hit = eval_cache_.find(fingerprint);
        if (hit != eval_cache_.end()) {
            return oval_retain(hit->second);
        }
    }

    OValue *source = req->data.request.source;
    if (!oval_is_thunk(source)) {
        throw std::runtime_error(std::string("exec_eval's Request source must be a Thunk, got ") + type_name(source));
    }

    OValueMap *bindings = scope_to_bindings({});
    OValue *result = registry_.exec(kind.lang, kind.env_id, maybe(source->data.nix_expr.body), bindings, find_shim(kind.lang));
    if (kind.env_id == std::numeric_limits<uint32_t>::max()) {
        registry_.cleanup_env(kind.lang, std::numeric_limits<uint32_t>::max());
    }
    if (kind.cacheable) {
        auto it = eval_cache_.find(fingerprint);
        if (it != eval_cache_.end()) {
            oval_release(it->second);
            it->second = oval_retain(result);
        } else {
            eval_cache_.emplace(fingerprint, oval_retain(result));
        }
    }
    return result;
}

OValue *Evaluator::eval_source(const std::string &src) {
    StringSet *backends = string_set_new();
    for (const auto &backend : registered_backends_) {
        string_set_add(backends, backend.c_str());
    }
    OParser parser{};
    parser_init(&parser, src.c_str(), backends);
    ONodeList *nodes = parser_parse(&parser);
    string_set_free(backends);
    if (nodes == nullptr) {
        throw std::runtime_error(std::string("failed to parse quoted source: ") + parser.error_msg);
    }
    try {
        OValue *result = eval_document(nodes);
        onode_list_free(nodes);
        return result;
    } catch (...) {
        onode_list_free(nodes);
        throw;
    }
}

OValue *Evaluator::eval_document(ONodeList *nodes) {
    std::map<std::string, OValue *> scope;
    OValue *last = oval_null();
    const std::size_t len = nodes == nullptr ? 0U : nodes->len;
    ONode **items = nodes == nullptr ? nullptr : nodes->items;

    try {
        for (std::size_t i = 0; i < len; ++i) {
            ONode *node = items[i];
            const bool whitespace_raw = node != nullptr && node->tag == ONODE_RAW_TEXT && whitespace_only(node->data.text);
            OValue *value = nullptr;
            if (node != nullptr && node->tag == ONODE_LET_BINDING) {
                value = eval_node(node->data.let_binding.expr, scope);
                auto it = scope.find(maybe(node->data.let_binding.name));
                if (it != scope.end()) {
                    oval_release(it->second);
                    it->second = oval_retain(value);
                } else {
                    scope.emplace(maybe(node->data.let_binding.name), oval_retain(value));
                }
            } else {
                value = eval_node(node, scope);
            }
            if (!oval_is_null(value) && !whitespace_raw) {
                oval_release(last);
                last = oval_retain(value);
            }
            oval_release(value);
        }

        if (policy_ == Policy::Autonomous) {
            flush_autonomous_buffer();
            if (is_schedulable_request(last)) {
                if (OValue *resolved = resolve_from_cache(last)) {
                    oval_release(last);
                    last = resolved;
                }
            }
        }
    } catch (...) {
        oval_release(last);
        for (auto &entry : scope) {
            oval_release(entry.second);
        }
        throw;
    }

    for (auto &entry : scope) {
        oval_release(entry.second);
    }
    return last;
}

OValue *Evaluator::eval_node(ONode *node, std::map<std::string, OValue *> &scope) {
    if (node == nullptr) {
        return oval_null();
    }
    switch (node->tag) {
    case ONODE_LET_BINDING:
        return eval_node(node->data.let_binding.expr, scope);
    case ONODE_RAW_TEXT:
        return oval_str(node->data.text == nullptr ? "" : node->data.text);
    case ONODE_VAR_REF: {
        auto it = scope.find(maybe(node->data.var_name));
        if (it == scope.end()) {
            throw std::runtime_error("Undefined variable: $" + maybe(node->data.var_name));
        }
        return oval_retain(it->second);
    }
    case ONODE_TYPED_EXPR:
        return eval_typed_expr(maybe(node->data.typed_expr.lang), node->data.typed_expr.env_id,
                               node->data.typed_expr.attr, node->data.typed_expr.body,
                               node->data.typed_expr.body_len, scope);
    case ONODE_CALL:
        return eval_call(maybe(node->data.call.fn_name), node->data.call.args,
                         node->data.call.args_len, scope);
    }
    throw std::runtime_error("unknown node tag");
}

OValue *Evaluator::eval_call(const std::string &fn_name, ONode **args, size_t args_len,
                             std::map<std::string, OValue *> &scope) {
    if (fn_name == "lazy") {
        if (args_len != 1U) {
            throw std::runtime_error("lazy(expr) takes exactly 1 argument, got " + std::to_string(args_len));
        }
        Policy saved = policy_;
        policy_ = Policy::Lazy;
        try {
            OValue *result = eval_node(args[0], scope);
            policy_ = saved;
            return result;
        } catch (...) {
            policy_ = saved;
            throw;
        }
    }

    if (fn_name == "autonomous") {
        if (args_len != 1U) {
            throw std::runtime_error("autonomous(expr) takes exactly 1 argument, got " + std::to_string(args_len));
        }
        Policy saved = policy_;
        policy_ = Policy::Autonomous;
        try {
            OValue *value = eval_node(args[0], scope);
            policy_ = saved;
            try {
                flush_autonomous_buffer();
            } catch (...) {
                oval_release(value);
                throw;
            }
            if (is_schedulable_request(value)) {
                if (OValue *resolved = resolve_from_cache(value)) {
                    oval_release(value);
                    return resolved;
                }
            }
            return value;
        } catch (...) {
            policy_ = saved;
            for (OValue *v : autonomous_buffer_) {
                oval_release(v);
            }
            autonomous_buffer_.clear();
            throw;
        }
    }

    std::vector<OValue *> arg_vals;
    try {
        for (std::size_t i = 0; i < args_len; ++i) {
            arg_vals.push_back(eval_node(args[i], scope));
        }

        if (fn_name == "instantiate") {
            if (arg_vals.size() != 1U) throw std::runtime_error("instantiate(expr) takes exactly 1 argument, got " + std::to_string(arg_vals.size()));
            OValue *req = oval_request(make_request_kind(REQ_INSTANTIATE), arg_vals[0]);
            OValue *out = auto_resolve(req);
            for (OValue *arg : arg_vals) oval_release(arg);
            return out;
        }
        if (fn_name == "realise") {
            if (arg_vals.size() != 1U) throw std::runtime_error("realise(drv) takes exactly 1 argument, got " + std::to_string(arg_vals.size()));
            OValue *req = oval_request(make_request_kind(REQ_REALISE), arg_vals[0]);
            OValue *out = auto_resolve(req);
            for (OValue *arg : arg_vals) oval_release(arg);
            return out;
        }
        if (fn_name == "now") {
            if (arg_vals.size() != 1U) throw std::runtime_error("now(req) takes exactly 1 argument, got " + std::to_string(arg_vals.size()));
            if (!oval_is_request(arg_vals[0])) throw std::runtime_error(std::string("now(req) expected a Request, got ") + type_name(arg_vals[0]));
            OValue *out = force_request(arg_vals[0]);
            for (OValue *arg : arg_vals) oval_release(arg);
            return out;
        }
        if (fn_name == "activate" || fn_name == "dry_activate") {
            if (arg_vals.empty() || arg_vals.size() > 2U) {
                throw std::runtime_error(fn_name + "(path) or " + fn_name + "(path, profile) — takes 1 or 2 args, got " + std::to_string(arg_vals.size()));
            }
            std::string profile = kDefaultSystemProfile;
            if (arg_vals.size() == 2U) {
                if (arg_vals[1]->tag == OVAL_STR || arg_vals[1]->tag == OVAL_SYSTEM) {
                    profile = maybe(arg_vals[1]->data.str_val);
                } else {
                    throw std::runtime_error(fn_name + "'s second arg must be a string profile path or a System value, got " + type_name(arg_vals[1]));
                }
            }
            const bool dry_run = fn_name == "dry_activate";
            OValue *req = oval_request(make_activate_kind(profile, dry_run), arg_vals[0]);
            OValue *out = auto_resolve(req);
            for (OValue *arg : arg_vals) oval_release(arg);
            return out;
        }
        if (fn_name == "current_system") {
            if (!arg_vals.empty()) throw std::runtime_error("current_system() takes no arguments, got " + std::to_string(arg_vals.size()));
            for (OValue *arg : arg_vals) oval_release(arg);
            return oval_system(kDefaultSystemProfile);
        }
        throw std::runtime_error("Unknown built-in function: `" + fn_name + "(...)`");
    } catch (...) {
        for (OValue *arg : arg_vals) {
            oval_release(arg);
        }
        throw;
    }
}

OValue *Evaluator::eval_typed_expr(const std::string &lang, uint32_t env_id,
                                   const char *attr, ONode **body, size_t body_len,
                                   std::map<std::string, OValue *> &scope) {
    if (lang == "quote") {
        return oval_expr(to_owned(::reconstruct_source(body, body_len)).c_str());
    }

    const BlockOptions options = parse_block_options(attr, lang);
    if (options.lazy || options.defer) {
        if (options.lazy) {
            if (lang == "nix_expr") {
                throw std::runtime_error("`nix_expr{lazy}^` is redundant — nix_expr^ is already lazy. Use bare nix_expr^, or use nix{lazy}^ if you want a generic deferred Nix eval.");
            }
            if (!is_pure_backend(lang)) {
                throw std::runtime_error("`" + lang + "{lazy}^` is invalid because " + lang + " is not a pure backend; caching a thunk that re-runs with side effects would be unsound. Use `" + lang + "{defer}^` instead — it captures the same thunk but never caches and always re-runs on force.");
            }
        } else if (options.defer) {
            if (lang == "nix_expr") {
                throw std::runtime_error("`nix_expr{defer}^` is redundant — nix_expr^ is already lazy. If you want a non-cacheable deferred Nix eval, write nix{defer}^.");
            }
        }
    }

    if (lang == "O") {
        OValue *last = oval_null();
        try {
            for (std::size_t i = 0; i < body_len; ++i) {
                ONode *child = body[i];
                if (child != nullptr && child->tag == ONODE_RAW_TEXT && whitespace_only(child->data.text)) {
                    continue;
                }
                OValue *value = nullptr;
                switch (child->tag) {
                case ONODE_RAW_TEXT: value = oval_str(child->data.text == nullptr ? "" : child->data.text); break;
                case ONODE_VAR_REF: {
                    auto it = scope.find(maybe(child->data.var_name));
                    if (it == scope.end()) throw std::runtime_error("Undefined variable: $" + maybe(child->data.var_name));
                    value = oval_retain(it->second);
                    break;
                }
                case ONODE_TYPED_EXPR:
                    value = eval_typed_expr(maybe(child->data.typed_expr.lang), child->data.typed_expr.env_id,
                                            child->data.typed_expr.attr, child->data.typed_expr.body,
                                            child->data.typed_expr.body_len, scope);
                    break;
                case ONODE_CALL:
                    value = eval_call(maybe(child->data.call.fn_name), child->data.call.args,
                                      child->data.call.args_len, scope);
                    break;
                case ONODE_LET_BINDING:
                    throw std::runtime_error("let bindings are only supported at document top level for now");
                }
                if (!oval_is_null(value)) {
                    oval_release(last);
                    last = oval_retain(value);
                }
                oval_release(value);
            }
            return last;
        } catch (...) {
            oval_release(last);
            throw;
        }
    }

    std::string buf;
    std::vector<OValue *> deps;
    const bool constructs_thunk = lang == "nix_expr" || options.lazy || options.defer;

    try {
        for (std::size_t i = 0; i < body_len; ++i) {
            ONode *child = body[i];
            switch (child->tag) {
            case ONODE_LET_BINDING:
                throw std::runtime_error("let bindings are only supported at document top level for now");
            case ONODE_RAW_TEXT:
                buf += maybe(child->data.text);
                break;
            case ONODE_VAR_REF: {
                auto it = scope.find(maybe(child->data.var_name));
                if (it == scope.end()) throw std::runtime_error("Undefined variable: $" + maybe(child->data.var_name));
                OValue *resolved = resolve_for_splice(oval_retain(it->second));
                buf += render_child(lang, resolved);
                if (constructs_thunk) deps.push_back(oval_retain(resolved));
                oval_release(resolved);
                break;
            }
            case ONODE_TYPED_EXPR: {
                OValue *child_val = eval_typed_expr(maybe(child->data.typed_expr.lang), child->data.typed_expr.env_id,
                                                    child->data.typed_expr.attr, child->data.typed_expr.body,
                                                    child->data.typed_expr.body_len, scope);
                OValue *resolved = resolve_for_splice(child_val);
                buf += render_child(lang, resolved);
                if (constructs_thunk) deps.push_back(oval_retain(resolved));
                oval_release(resolved);
                break;
            }
            case ONODE_CALL: {
                OValue *raw = eval_call(maybe(child->data.call.fn_name), child->data.call.args,
                                        child->data.call.args_len, scope);
                OValue *resolved = resolve_for_splice(raw);
                buf += render_child(lang, resolved);
                if (constructs_thunk) deps.push_back(oval_retain(resolved));
                oval_release(resolved);
                break;
            }
            }
        }

        if (options.lazy || options.defer) {
            std::vector<OValue *> dep_copy = deps;
            OValue *thunk = oval_thunk(buf.c_str(), dep_copy.data(), dep_copy.size());
            OValue *req = oval_request(make_eval_kind(lang, env_id, options.lazy), thunk);
            oval_release(thunk);
            for (OValue *dep : deps) oval_release(dep);
            return req;
        }

        if (lang == "nix_expr") {
            std::vector<OValue *> dep_copy = deps;
            OValue *out = oval_nix_expr(buf.c_str(), dep_copy.data(), dep_copy.size());
            for (OValue *dep : deps) oval_release(dep);
            return out;
        }
        if (lang == "html") {
            for (OValue *dep : deps) oval_release(dep);
            return oval_html(buf.c_str());
        }
        if (lang == "markdown" || lang == "md" || lang == "text" || lang == "plain" || lang == "latex" || lang == "tex") {
            for (OValue *dep : deps) oval_release(dep);
            return oval_str(buf.c_str());
        }

        OValueMap *bindings = scope_to_bindings(scope);
        registry_.send_exec(lang, env_id, buf, bindings, find_shim(lang));
        OValue *result = nullptr;
        while (true) {
            ExecStep step = registry_.recv_exec_step(lang, env_id);
            if (step.kind == ExecStepKind::Done) {
                result = step.value;
                break;
            }
            OValue *inner = eval_source(step.src);
            registry_.send_eval_result(lang, env_id, inner);
            oval_release(inner);
        }
        if (env_id == std::numeric_limits<uint32_t>::max()) {
            registry_.cleanup_env(lang, std::numeric_limits<uint32_t>::max());
        }
        for (OValue *dep : deps) oval_release(dep);
        return result;
    } catch (...) {
        for (OValue *dep : deps) oval_release(dep);
        if (env_id == std::numeric_limits<uint32_t>::max()) {
            try { registry_.cleanup_env(lang, std::numeric_limits<uint32_t>::max()); } catch (...) {}
        }
        throw;
    }
}

std::string Evaluator::render_child(const std::string &lang, OValue *val) {
    if (lang == "python" || lang == "py") return render_python(val);
    if (lang == "html") return render_html(val);
    if (lang == "latex" || lang == "tex") return render_latex(val);
    if (lang == "markdown" || lang == "md") return render_markdown(val);
    if (lang == "nix" || lang == "nix_store" || lang == "nixos_test") return render_nix(val);
    return splice_repr(val);
}

std::string Evaluator::find_shim(const std::string &lang) {
    namespace fs = std::filesystem;
    const std::array<std::string, 4> candidates = {
        lang + "_shim.py",
        lang + "_shim",
        lang + ".py",
        lang
    };
    for (const auto &candidate : candidates) {
        fs::path path = fs::path(shim_dir_) / candidate;
        if (fs::exists(path)) {
            return path.string();
        }
    }
    return (fs::path(shim_dir_) / (lang + "_shim.py")).string();
}

} // namespace olang
