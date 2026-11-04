// rtlog from C++: RAII spans, std::thread and std::string arguments.
//
//   c++ -O2 -std=c++17 -pthread -I c/include c/examples/demo.cpp -o demo_cpp
//   ./demo_cpp demo_cpp.rtlog
//   rtlog contract --elf demo_cpp -o demo_cpp.contract.jsonl
//   rtlog decode --contract demo_cpp.contract.jsonl --perfetto -o demo_cpp.json demo_cpp.rtlog
#define RTLOG_IMPLEMENTATION
#include "rtlog.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <string>
#include <thread>
#include <vector>

static int write_file(void *user, const uint8_t *bytes, size_t len) {
    return std::fwrite(bytes, 1, len, static_cast<FILE *>(user)) == len ? 0 : 1;
}

static void handle(int id, const std::string &route) {
    RTLOG_SPAN(req, "request");
    RTLOG_INFO("request id={} route={}", id, route);
    {
        RTLOG_SPAN(q, "db.query");
        std::this_thread::sleep_for(std::chrono::microseconds(300 + id * 20));
        RTLOG_DEBUG("rows={} cached={}", id * 3, id % 2 == 0);
    }
}

int main(int argc, char **argv) {
    FILE *f = std::fopen(argc > 1 ? argv[1] : "demo_cpp.rtlog", "wb");
    if (!f) return 1;
    std::atomic<bool> stop{false};
    std::thread drain([&] {
        while (!stop) {
            rtlog_drain(write_file, f);
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        }
    });
    std::vector<std::thread> workers;
    for (int w = 0; w < 3; w++) {
        workers.emplace_back([w] {
            std::string name = "worker-" + std::to_string(w);
            rtlog_set_thread_name(name.c_str());
            for (int i = 0; i < 10; i++) handle(w * 100 + i, i % 3 ? "/api/orders" : "/health");
            rtlog_thread_exit();
        });
    }
    for (auto &t : workers) t.join();
    RTLOG_WARN("shutdown pending={} ok={}", 0u, true);
    stop = true;
    drain.join();
    rtlog_drain(write_file, f);
    std::fclose(f);
    return 0;
}
