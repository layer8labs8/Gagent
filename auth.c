/*
 * G AGENT (v3.1) - Direct Gemini API Key Vault (Zero-Client-ID Mode)
 * File: src/auth.c
 */

#include "gagent.h"
#include <string.h>
#include <stdio.h>

void auth_vault_init(GeminiApiVault *vault, const char *vault_path) {
    if (!vault) return;
    memset(vault, 0, sizeof(GeminiApiVault));
    strncpy(vault->active_model, "gemini-2.5-pro", sizeof(vault->active_model) - 1);
    if (vault_path) {
        strncpy(vault->vault_path, vault_path, GAGENT_MAX_PATH - 1);
    }
}

int auth_link_api_key(GeminiApiVault *vault, const char *raw_key, GagentError *err) {
    if (!vault || !raw_key || strlen(raw_key) < 6) {
        gagent_error_init(err, GAGENT_ERR_AUTH, LINK_TOKEN_VAULT, LINK_ENGINE_RUNTIME,
                          401, "", "Invalid Gemini API Key: must be at least 6 characters");
        return -1;
    }

    strncpy(vault->api_key, raw_key, sizeof(vault->api_key) - 1);
    vault->is_linked = 1;

    if (vault->vault_path[0] != '\0') {
        FILE *f = fopen(vault->vault_path, "wb");
        if (f) {
            fprintf(f, "{\n  \"provider\": \"gemini-direct-api\",\n  \"model\": \"%s\",\n  \"api_key\": \"%s\",\n  \"status\": \"LINKED\"\n}\n",
                    vault->active_model, vault->api_key);
            fclose(f);
        }
    }
    return 0;
}

int auth_verify_key(const GeminiApiVault *vault, GagentError *err) {
    if (!vault || !vault->is_linked || strlen(vault->api_key) < 6) {
        gagent_error_init(err, GAGENT_ERR_AUTH, LINK_TOKEN_VAULT, LINK_WORKSPACE_BRIDGE,
                          401, "", "No valid Gemini API Key linked in vault");
        return -1;
    }
    return 0;
}
