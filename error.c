/*
 * G AGENT (v3.1) - Unified Fault Taxonomy & Diagnostic Descriptor Engine
 * File: src/error.c
 */

#include "gagent.h"
#include <string.h>
#include <stdio.h>

const char* gagent_link_name(SubsystemLink link) {
    switch (link) {
        case LINK_AUTH_LOOPBACK:    return "auth::loopback";
        case LINK_TOKEN_VAULT:      return "auth::api_key_vault";
        case LINK_WORKSPACE_BRIDGE: return "workspace::drive_zero_disk";
        case LINK_LOCAL_FILESYSTEM: return "tools::filesystem_guard";
        case LINK_PROCESS_SANDBOX:  return "sandbox::rootfs_verifier";
        case LINK_ENGINE_RUNTIME:   return "engine::runtime_dispatcher";
        default:                    return "unknown::link";
    }
}

const char* gagent_strategy_name(RecoveryStrategy strategy) {
    switch (strategy) {
        case STRATEGY_NONE:               return "None";
        case STRATEGY_RELOAD_API_KEY:     return "ReloadApiKeyFromVault";
        case STRATEGY_REBIND_BRIDGE:      return "RebindDriveZeroDiskBridge";
        case STRATEGY_ROLLBACK_SNAPSHOT:  return "RollbackFileSnapshot";
        case STRATEGY_SANITIZE_AND_RETRY: return "SanitizeAndRetryProcess";
        case STRATEGY_BACKOFF_AND_RETRY:  return "BackoffAndRetryRequest";
        case STRATEGY_ESCALATE_TO_USER:   return "EscalateToUser";
        default:                          return "UnknownStrategy";
    }
}

void gagent_error_clear(GagentError *err) {
    if (!err) return;
    memset(err, 0, sizeof(GagentError));
    err->code = GAGENT_OK;
    err->recoverable = 1;
    err->suggested_action = STRATEGY_NONE;
}

void gagent_error_init(GagentError *err, GagentErrorCode code, SubsystemLink origin,
                       SubsystemLink target, int status, const char *path, const char *msg) {
    if (!err) return;
    memset(err, 0, sizeof(GagentError));
    err->code = code;
    err->origin = origin;
    err->target = target;
    err->http_or_exit_status = status;

    if (path) {
        strncpy(err->target_path, path, GAGENT_MAX_PATH - 1);
    }
    if (msg) {
        strncpy(err->message, msg, GAGENT_MAX_MSG - 1);
    }

    switch (code) {
        case GAGENT_OK:
            err->recoverable = 1;
            err->suggested_action = STRATEGY_NONE;
            break;
        case GAGENT_ERR_AUTH:
            err->recoverable = 1;
            err->suggested_action = STRATEGY_RELOAD_API_KEY;
            break;
        case GAGENT_ERR_LINKAGE:
            err->recoverable = 1;
            err->suggested_action = STRATEGY_REBIND_BRIDGE;
            break;
        case GAGENT_ERR_FILESYSTEM:
        case GAGENT_ERR_SANDBOX_VERIFY:
            err->recoverable = 1;
            err->suggested_action = STRATEGY_ROLLBACK_SNAPSHOT;
            break;
        case GAGENT_ERR_PROCESS:
            err->recoverable = (status == 124 || status == 137);
            err->suggested_action = err->recoverable ? STRATEGY_SANITIZE_AND_RETRY : STRATEGY_ESCALATE_TO_USER;
            break;
        case GAGENT_ERR_WORKSPACE_API:
            err->recoverable = (status == 401 || status == 408 || status == 429 || status >= 500);
            err->suggested_action = (status == 401) ? STRATEGY_RELOAD_API_KEY : STRATEGY_BACKOFF_AND_RETRY;
            break;
        case GAGENT_ERR_PATH_TRAVERSAL:
        case GAGENT_ERR_SESSION_ABORT:
        default:
            err->recoverable = 0;
            err->suggested_action = STRATEGY_ESCALATE_TO_USER;
            break;
    }
}

int gagent_error_is_recoverable(const GagentError *err) {
    if (!err) return 0;
    return err->recoverable;
}
