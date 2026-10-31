/*
 * c_smoke.c — C ABI smoke test of Block B4 (coupling-client).
 *
 * Usage: c_smoke <sim_id> <steps>
 *   sim_id accepts 0x-prefixed hex or decimal (same as examples/mock_host.rs).
 *
 * The host (examples/mock_host) drives `steps` lockstep iterations with
 * joint_position = n * 0.5 and expects joint_torque = 2 * joint_position.
 *
 * Exit code 0 on success; on failure the wcp_last_error() text is printed and
 * the process exits with 1.
 */

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "wcp_client.h"

#define TIMEOUT_MS 15000u
#define MAX_STEPS 1000u

static void fail(const char *what) {
    const char *err = wcp_last_error();
    if (err != NULL && err[0] != '\0') {
        fprintf(stderr, "c_smoke: %s: %s\n", what, err);
    } else {
        fprintf(stderr, "c_smoke: %s (no error message)\n", what);
    }
    exit(1);
}

static uint64_t parse_sim_id(const char *s) {
    char *end = NULL;
    uint64_t value;
    if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) {
        value = strtoull(s + 2, &end, 16);
    } else {
        value = strtoull(s, &end, 10);
    }
    if (end == s || *end != '\0') {
        fprintf(stderr, "c_smoke: invalid sim_id %s\n", s);
        exit(2);
    }
    return value;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: c_smoke <sim_id> <steps>\n");
        return 2;
    }
    const uint64_t sim_id = parse_sim_id(argv[1]);
    const unsigned long steps = strtoul(argv[2], NULL, 10);
    if (steps == 0 || steps > MAX_STEPS) {
        fprintf(stderr, "c_smoke: steps must be in 1..%u\n", MAX_STEPS);
        return 2;
    }

    /* Hello (protocol §9); field names are frozen. */
    static const char hello[] =
        "{\"protocol\":[1,0],"
        "\"model_name\":\"c-smoke\","
        "\"model_version\":\"0.1.0\","
        "\"steppable\":true,"
        "\"sync_modes\":[\"lockstep\"],"
        "\"internal_dt_ns\":100000,"
        "\"supported_signals\":[1,257],"
        "\"custom_signals\":[]}";

    wcp_client *client = NULL;
    if (wcp_connect(sim_id, 1, hello, TIMEOUT_MS, &client) != WCP_OK) {
        fail("connect");
    }

    const char *manifest = wcp_manifest_json(client);
    if (manifest == NULL) {
        fail("manifest_json");
    }
    if (strstr(manifest, "\"joint_position\"") == NULL ||
        strstr(manifest, "\"joint_torque\"") == NULL) {
        fprintf(stderr, "c_smoke: manifest does not contain both slots: %s\n", manifest);
        wcp_close(client);
        return 1;
    }

    wcp_slot position = 0;
    wcp_slot torque = 0;
    if (wcp_slot_by_name(client, "joint_position", &position) != WCP_OK) {
        fail("slot joint_position");
    }
    if (wcp_slot_by_name(client, "joint_torque", &torque) != WCP_OK) {
        fail("slot joint_torque");
    }
    wcp_slot missing = 0;
    if (wcp_slot_by_name(client, "no_such_slot", &missing) != WCP_TIMEOUT) {
        fprintf(stderr, "c_smoke: unknown slot must return WCP_TIMEOUT\n");
        wcp_close(client);
        return 1;
    }

    uint64_t n = 1;
    while (n <= steps) {
        wcp_step step;
        memset(&step, 0, sizeof step);
        int rc = wcp_wait_step(client, TIMEOUT_MS, &step);
        if (rc == WCP_RESET) {
            /* Reset wakeup: no step, only the new epoch. Reinitialise and acknowledge. */
            if (step.step_id != 0 || step.epoch_changed != 1) {
                fprintf(stderr, "c_smoke: malformed reset event (step_id=%" PRIu64
                                ", changed=%u)\n",
                        step.step_id, (unsigned)step.epoch_changed);
                wcp_close(client);
                return 1;
            }
            if (wcp_ack_reset(client) != WCP_OK) {
                fail("ack_reset");
            }
            continue;
        }
        if (rc != WCP_OK) {
            fail("wait_step");
        }
        if (step.step_id != n) {
            fprintf(stderr, "c_smoke: expected step_id %" PRIu64 ", got %" PRIu64 "\n", n,
                    step.step_id);
            wcp_close(client);
            return 1;
        }
        if (step.epoch_changed != 0) {
            fprintf(stderr, "c_smoke: unexpected reset flag on step %" PRIu64 "\n", n);
            wcp_close(client);
            return 1;
        }
        if (step.epoch != 0) {
            fprintf(stderr, "c_smoke: unexpected epoch %" PRIu64 " on step %" PRIu64 "\n",
                    step.epoch, n);
            wcp_close(client);
            return 1;
        }

        double p = 0.0;
        if (wcp_read_f64(client, position, &p) != WCP_OK) {
            fail("read_f64(joint_position)");
        }
        const double expected = (double)n * 0.5;
        if (p < expected - 1e-9 || p > expected + 1e-9) {
            fprintf(stderr, "c_smoke: step %" PRIu64 ": expected position %f, got %f\n", n,
                    expected, p);
            wcp_close(client);
            return 1;
        }

        if (wcp_write_f64(client, torque, 2.0 * p) != WCP_OK) {
            fail("write_f64(joint_torque)");
        }
        if (wcp_commit(client) != WCP_OK) {
            fail("commit");
        }
        ++n;
    }

    wcp_close(client);
    printf("C_SMOKE_OK steps=%lu\n", steps);
    return 0;
}
