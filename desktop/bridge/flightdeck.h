#include <stdint.h>
#include <stddef.h>

typedef struct Handle FlightdeckSession;
typedef struct Cancellation FlightdeckControl;
typedef struct { uint8_t *pointer; size_t length; } FlightdeckBytes;
typedef struct { uint32_t kind; const uint8_t *pointer; size_t length; } FlightdeckAppKey;

/* One serial owner per session. Copy every response before freeing it exactly once. */
FlightdeckSession *fd_open(const uint8_t *path, size_t length);
FlightdeckControl *fd_control_new(void);
uint64_t fd_control_epoch(const FlightdeckControl *control);
uint32_t fd_control_cancel(const FlightdeckControl *control);
void fd_control_free(FlightdeckControl *control);
FlightdeckSession *fd_open_controlled(const uint8_t *path, size_t length,
    const FlightdeckControl *control, uint64_t epoch);
FlightdeckSession *fd_open_live_controlled(const uint8_t *path, size_t length,
    const FlightdeckControl *control, uint64_t epoch);
FlightdeckBytes fd_live_poll(FlightdeckSession *session);
FlightdeckBytes fd_prepare(FlightdeckSession *session, uint64_t epoch);
FlightdeckBytes fd_catalog(FlightdeckSession *session);
FlightdeckBytes fd_select(FlightdeckSession *session, uint64_t second, uint32_t alternative);
FlightdeckBytes fd_follow(FlightdeckSession *session);
FlightdeckBytes fd_cpu_detail(FlightdeckSession *session);
FlightdeckBytes fd_apps(FlightdeckSession *session, uint32_t offset);
FlightdeckBytes fd_app_detail(FlightdeckSession *session, const FlightdeckAppKey *key,
    uint32_t offset);
FlightdeckBytes fd_app_history(FlightdeckSession *session, const FlightdeckAppKey *key);
FlightdeckBytes fd_page(FlightdeckSession *session, uint32_t table, uint64_t offset);
void fd_bytes_free(FlightdeckBytes bytes);
void fd_close(FlightdeckSession *session);
