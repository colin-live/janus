/*
 * wcp_client.h — C ABI of Block B4 (coupling-client), protocol v1.
 *
 * Return convention: 0 = success, 1 = timeout / value unavailable,
 * 2 = epoch reset (wcp_wait_step only), < 0 = error (message via wcp_last_error()).
 *
 * Threading: a wcp_client is single-threaded; do not call it from two threads at once.
 * All pointers returned by this header are owned by the client and stay valid until
 * wcp_close(), except wcp_last_error(), which is thread-local.
 */
#ifndef WCP_CLIENT_H
#define WCP_CLIENT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define WCP_OK 0
#define WCP_TIMEOUT 1
#define WCP_RESET 2
#define WCP_ERROR (-1)

typedef struct wcp_client wcp_client;

/* Opaque slot handle resolved at connect time. */
typedef uint32_t wcp_slot;

typedef struct wcp_step {
    uint64_t step_id;
    int64_t sim_time_ns;
    int64_t dt_ns;
    uint64_t epoch;
    uint8_t epoch_changed; /* 1 when wcp_wait_step returned WCP_RESET (then step_id == 0) */
} wcp_step;

/* Validity byte values (protocol §3). */
#define WCP_VALIDITY_MISSING 0
#define WCP_VALIDITY_VALID 1
#define WCP_VALIDITY_INVALID 2
#define WCP_VALIDITY_STALE 3
#define WCP_VALIDITY_OUT_OF_RANGE 4
#define WCP_VALIDITY_ERROR 5

/* Connect, send hello_json (protocol §9) and wait for Ready. */
int32_t wcp_connect(uint64_t sim_id, uint32_t instance_id, const char *hello_json,
                    uint32_t timeout_ms, wcp_client **out);

/* Close the client (NULL is a no-op). */
void wcp_close(wcp_client *client);

/* Manifest JSON (protocol §8) of the connected segment, NUL-terminated. */
const char *wcp_manifest_json(const wcp_client *client);

/* Resolve a slot name: 0 and *out set, 1 if unknown / disabled, < 0 on error. */
int32_t wcp_slot_by_name(const wcp_client *client, const char *name, wcp_slot *out);

/*
 * Wait for a step: 0 and *out set, 1 on timeout, < 0 on error.
 * Returns WCP_RESET when the epoch changed: *out carries only the new epoch
 * (step_id == 0, epoch_changed == 1). Reinitialise all internal state, then call
 * wcp_ack_reset() and do NOT commit; the adapter waits for the ack before it
 * publishes the first step of the new epoch.
 */
int32_t wcp_wait_step(wcp_client *client, uint32_t timeout_ms, wcp_step *out);

/* Acknowledge the reset reported by wcp_wait_step (writes model_epoch_ack). */
int32_t wcp_ack_reset(wcp_client *client);

/* Read a slot: 0 and *out set, 1 if unavailable (wrong type / validity != Valid), < 0. */
int32_t wcp_read_f64(const wcp_client *client, wcp_slot slot, double *out);
int32_t wcp_read_vec3(const wcp_client *client, wcp_slot slot, double out[3]);
int32_t wcp_read_quat(const wcp_client *client, wcp_slot slot, double out[4]);

/* Write effort values (non-finite values mark the slot Invalid, protocol §10). */
int32_t wcp_write_f64(wcp_client *client, wcp_slot slot, double value);
int32_t wcp_write_vec3(wcp_client *client, wcp_slot slot, const double value[3]);

/* Set the validity byte of an effort slot (e.g. WCP_VALIDITY_ERROR). */
int32_t wcp_invalidate(wcp_client *client, wcp_slot slot, uint8_t validity);

/* Publish the pending step's effort (heartbeat, effort_step_id, m2s wakeup). */
int32_t wcp_commit(wcp_client *client);

/* Last error message on the calling thread (empty string when none). */
const char *wcp_last_error(void);

#ifdef __cplusplus
}
#endif

#endif /* WCP_CLIENT_H */
