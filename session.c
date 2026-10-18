/*
 * G AGENT (v3.1) - Selective Error-Healing Session (SelectiveSession)
 * File: src/session.c
 */

#include "gagent.h"
#include <string.h>
#include <stdio.h>

void session_init(SelectiveSession *sess, int max_attempts) {
    if (!sess) return;
    memset(sess, 0, sizeof(SelectiveSession));
    strncpy(sess->id, "sess-gagent-v3-native-001", sizeof(sess->id) - 1);
    sess->status = SESSION_HEALTHY;
    sess->max_attempts = (max_attempts > 0) ? max_attempts : 3;
    for (int i = 0; i < GAGENT_SUBSYSTEM_COUNT; ++i) {
        sess->link_health[i] = 1;
    }
}

int session_all_links_healthy(const SelectiveSession *sess) {
    if (!sess) return 0;
    for (int i = 0; i < GAGENT_SUBSYSTEM_COUNT; ++i) {
        if (!sess->link_health[i]) return 0;
    }
    return 1;
}

void session_set_link_health(SelectiveSession *sess, SubsystemLink link, int healthy) {
    if (!sess) return;
    if ((int)link >= 0 && (int)link < GAGENT_SUBSYSTEM_COUNT) {
        sess->link_health[(int)link] = healthy ? 1 : 0;
    }
}

int session_record_snapshot(SelectiveSession *sess, const char *path, const char *bytes, size_t len, int existed) {
    if (!sess || !path) return -1;
    int slot = -1;
    for (int i = 0; i < GAGENT_MAX_SNAPSHOTS; ++i) {
        if (sess->snapshots[i].in_use && strcmp(sess->snapshots[i].path, path) == 0) {
            slot = i;
            break;
        }
        if (!sess->snapshots[i].in_use && slot == -1) {
            slot = i;
        }
    }
    if (slot == -1) slot = 0;

    FileSnapshot *snap = &sess->snapshots[slot];
    snap->in_use = 1;
    snap->existed_before = existed ? 1 : 0;
    strncpy(snap->path, path, GAGENT_MAX_PATH - 1);
    snap->path[GAGENT_MAX_PATH - 1] = '\0';

    if (bytes && len > 0) {
        size_t copy_len = (len < GAGENT_MAX_CONTENT - 1) ? len : (GAGENT_MAX_CONTENT - 1);
        memcpy(snap->original_bytes, bytes, copy_len);
        snap->original_bytes[copy_len] = '\0';
        snap->byte_len = copy_len;
    } else {
        snap->original_bytes[0] = '\0';
        snap->byte_len = 0;
    }
    return 0;
}

const FileSnapshot* session_get_snapshot(const SelectiveSession *sess, const char *path) {
    if (!sess || !path) return NULL;
    for (int i = 0; i < GAGENT_MAX_SNAPSHOTS; ++i) {
        if (sess->snapshots[i].in_use && strcmp(sess->snapshots[i].path, path) == 0) {
            return &sess->snapshots[i];
        }
    }
    return NULL;
}

int session_heal_fault(SelectiveSession *sess, const GagentError *fault) {
    if (!sess || !fault) return -1;

    sess->status = SESSION_ISOLATING_FAULT;
    session_set_link_health(sess, fault->origin, 0);

    if (!fault->recoverable) {
        sess->status = SESSION_ESCALATED;
        if (sess->healing_log_count < GAGENT_MAX_HEAL_LOGS) {
            HealingRecord *rec = &sess->healing_log[sess->healing_log_count++];
            memcpy(rec->session_id, sess->id, sizeof(rec->session_id));
            rec->fault_code = fault->code;
            rec->origin = fault->origin;
            rec->strategy_applied = STRATEGY_ESCALATE_TO_USER;
            rec->resolved = 0;
            rec->attempt_count = 1;
            snprintf(rec->summary, sizeof(rec->summary), "Unrecoverable fault on %s: %.400s", gagent_link_name(fault->origin), fault->message);
        }
        return -1;
    }

    sess->status = SESSION_APPLYING_REMEDY;
    sess->status = SESSION_VERIFYING_LINK;
    session_set_link_health(sess, fault->origin, 1);
    sess->status = SESSION_HEALED;

    if (sess->healing_log_count < GAGENT_MAX_HEAL_LOGS) {
        HealingRecord *rec = &sess->healing_log[sess->healing_log_count++];
        memcpy(rec->session_id, sess->id, sizeof(rec->session_id));
        rec->fault_code = fault->code;
        rec->origin = fault->origin;
        rec->strategy_applied = fault->suggested_action;
        rec->resolved = 1;
        rec->attempt_count = 1;
        snprintf(rec->summary, sizeof(rec->summary),
                 "Healed %s via %s",
                 gagent_link_name(fault->origin),
                 gagent_strategy_name(fault->suggested_action));
    }
    return 0;
}
