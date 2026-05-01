import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

export interface CachedQuota {
    five_hour_left: number;
    five_hour_reset: string;
    five_hour_reset_at?: number;
    five_hour_label?: string;
    weekly_left: number;
    weekly_reset: string;
    weekly_reset_at?: number;
    weekly_label?: string;
    plan_type: string;
    is_valid_for_cli?: boolean;
    updated_at: string;
}

export interface AppSettings {
    auto_reload_ide: boolean;
    primary_ide: string;
    use_pkill_restart: boolean;
    background_refresh: boolean;
    refresh_interval_minutes: number;
    inactive_refresh_days: number;
    theme_palette: string;
}

export interface KeepaliveState {
    inactive_refresh_enabled: boolean;
    last_attempt_at: string | null;
    last_success_at: string | null;
    last_error: string | null;
}

export interface SyncStatus {
    is_synced: boolean;
    disk_email: string | null;
    matching_id: string | null;
    current_id: string | null;
}

export interface Account {
    id: string;
    name: string;
    auth_json: unknown;
    created_at: string;
    last_used: string | null;
    notes: string | null;
    cached_quota: CachedQuota | null;
    keepalive: KeepaliveState;
    is_banned: boolean;
    is_token_invalid: boolean;
    is_logged_out: boolean;
}

export function useAccounts() {
    const [accounts, setAccounts] = useState<Account[]>([]);
    const [currentId, setCurrentId] = useState<string | null>(null);
    const [settings, setSettings] = useState<AppSettings>({
        auto_reload_ide: false,
        primary_ide: 'Windsurf',
        use_pkill_restart: false,
        background_refresh: false,
        refresh_interval_minutes: 30,
        inactive_refresh_days: 7,
        theme_palette: 'midnight',
    });
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);

    // Load accounts and settings
    const loadData = useCallback(async () => {
        try {
            setError(null);

            const [accountList, current, appSettings] = await Promise.all([
                invoke<Account[]>('get_accounts'),
                invoke<string | null>('get_current_account_id'),
                invoke<AppSettings>('get_settings'),
            ]);

            setAccounts(accountList);
            setCurrentId(current);
            setSettings(appSettings);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    }, []);

    // Initial load
    useEffect(() => {
        loadData();
    }, [loadData]);

    const setInactiveRefreshEnabled = useCallback(async (id: string, enabled: boolean) => {
        try {
            setError(null);
            await invoke('set_account_inactive_refresh_enabled', { id, enabled });
            await loadData();
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, [loadData]);

    // Update settings
    const updateSettings = useCallback(async (newSettings: AppSettings) => {
        try {
            setError(null);
            await invoke('update_settings', { settings: newSettings });
            setSettings(newSettings);
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, []);

    // ... other methods remain unchanged, but use loadData instead of loadAccounts ...

    // Import current account
    const importCurrent = useCallback(async (name: string, notes?: string) => {
        try {
            setError(null);
            await invoke('import_current_account', { name, notes });
            await loadData();
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, [loadData]);

    // Switch account
    const switchTo = useCallback(async (id: string) => {
        try {
            setError(null);
            await invoke('switch_account', { id });
            setCurrentId(id);
            await loadData();
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, [loadData]);

    // Delete account (with confirmation)
    const deleteAccount = useCallback(async (id: string) => {
        const account = accounts.find(a => a.id === id);
        const name = account?.name || id;
        if (!window.confirm(`Are you sure you want to delete account ${name}?`)) return;
        try {
            setError(null);
            if (currentId === id) {
                setCurrentId(null);
            }
            await invoke('delete_account', { id });
            await loadData();
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, [loadData, accounts, currentId]);

    // Update account
    const updateAccount = useCallback(async (id: string, name?: string, notes?: string) => {
        try {
            setError(null);
            await invoke('update_account', { id, name, notes });
            await loadData();
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, [loadData]);

    // Export
    const exportAccounts = useCallback(async () => {
        try {
            return await invoke<string>('export_accounts');
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, []);

    // Import
    const importAccounts = useCallback(async (json: string) => {
        try {
            setError(null);
            await invoke('import_accounts', { json });
            await loadData();
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, [loadData]);

    // Check Codex login status
    const checkCodexLogin = useCallback(async () => {
        try {
            return await invoke<boolean>('check_codex_login');
        } catch {
            return false;
        }
    }, []);

    // Start OAuth login
    const startOAuthLogin = useCallback(async () => {
        try {
            setError(null);
            return await invoke<string>('start_oauth_login');
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, []);

    // Complete OAuth login
    const finalizeOAuthLogin = useCallback(async (code: string) => {
        try {
            setError(null);
            const account = await invoke<Account>('finalize_oauth_login', { code });
            await loadData();
            return account;
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, [loadData]);

    // Reload IDE window
    const reloadIdeWindows = useCallback(async (useWindowReload: boolean = false) => {
        try {
            setError(null);
            return await invoke<string[]>('reload_ide_windows', { useWindowReload });
        } catch (err) {
            setError(String(err));
            throw err;
        }
    }, []);

    return {
        accounts,
        currentId,
        settings,
        loading,
        error,
        refresh: loadData,
        importCurrent,
        switchTo,
        deleteAccount,
        updateAccount,
        exportAccounts,
        importAccounts,
        checkCodexLogin,
        startOAuthLogin,
        finalizeOAuthLogin,
        reloadIdeWindows,
        updateSettings,
        setInactiveRefreshEnabled,
        checkSyncConflict: useCallback(async () => {
            return invoke<string | null>('check_sync_conflict');
        }, []),
        getSyncStatus: useCallback(async () => {
            return invoke<SyncStatus>('get_sync_status');
        }, []),
        syncActiveWithDisk: useCallback(async () => {
            await invoke('sync_active_with_disk');
            await loadData();
        }, [loadData]),
    };
}
