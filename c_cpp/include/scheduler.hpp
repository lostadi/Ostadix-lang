#ifndef O_LANG_SCHEDULER_HPP
#define O_LANG_SCHEDULER_HPP

#include <string>
#include <map>
#include <vector>
#include <set>
#include <memory>
#include <functional>

extern "C" {
#include "value.h"
}

namespace olang {

/* ── DiskCache ────────────────────────────────────────────────────────── */
class DiskCache {
public:
    explicit DiskCache(const std::string &dir);
    static std::string default_dir();
    OValue *get(const std::string &fingerprint);
    void put(const std::string &fingerprint, OValue *value);
private:
    std::string dir_;
};

/* ── AutonomousScheduler ──────────────────────────────────────────────── */
class AutonomousScheduler {
public:
    AutonomousScheduler();
    explicit AutonomousScheduler(const std::string &cache_dir);
    ~AutonomousScheduler() = default;

    void set_parallelism(size_t n);

    OValue *cache_get(const std::string &fingerprint);
    OValue *execute(OValue *req);
    std::map<std::string, OValue *> execute_batch(
        const std::vector<OValue *> &roots,
        std::function<OValue *(OValue *)> eval_fn = nullptr);

    // Public for test access
    std::map<std::string, OValue *> mem_cache;

private:
    void cache_put(const std::string &fingerprint, OValue *value);
    std::unique_ptr<DiskCache> disk_cache_;
    size_t parallelism_;
};

// Dependency graph helpers
void collect_transitive_requests(OValue *req, std::map<std::string, OValue *> &out);

} // namespace olang
#endif
