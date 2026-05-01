import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Copy, Check, Save } from 'lucide-react';
import './Proxy.css';

interface ProxyStatus {
    enabled: boolean;
    port: number;
    is_running: boolean;
    base_url: string;
    allow_lan: boolean;
    lan_base_url?: string | null;
    total_requests: number;
    auto_switches: number;
}

interface AppSettings {
    auto_reload_ide: boolean;
    primary_ide: string;
    use_pkill_restart: boolean;
    background_refresh: boolean;
    refresh_interval_minutes: number;
    inactive_refresh_days: number;
    theme_palette: string;
    allow_auto_switch_to_free: boolean;
    proxy_enabled: boolean;
    proxy_port: number;
    proxy_allow_lan: boolean;
    proxy_threshold_5h: number;
    proxy_threshold_weekly: number;
    proxy_free_guard: number;
    notify_on_switch: boolean;
    inject_switch_message: boolean;
    quota_refresh_enabled: boolean;
    quota_refresh_interval: number;
    quota_refresh_batch: number;
}

export function Proxy() {
    const [status, setStatus] = useState<ProxyStatus | null>(null);
    const [settings, setSettings] = useState<AppSettings | null>(null);
    const [port, setPort] = useState(18080);
    const [copied, setCopied] = useState(false);
    const [saving, setSaving] = useState(false);
    const [envWriting, setEnvWriting] = useState(false);
    const [killing, setKilling] = useState(false);
    const [message, setMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);
    const [switchedAccount, setSwitchedAccount] = useState<string | null>(null);
    const [fastMode, setFastMode] = useState(false);

    const fetchAll = async () => {
        try {
            const [s, st, fm] = await Promise.all([
                invoke<AppSettings>('get_settings'),
                invoke<ProxyStatus>('get_proxy_status'),
                invoke<boolean>('get_codex_fast_mode'),
            ]);
            setSettings(s);
            setFastMode(fm);
            setStatus(st);
            setPort(s.proxy_port);
        } catch (e) {
            console.error('Failed to load proxy status:', e);
        }
    };

    useEffect(() => {
        fetchAll();
        const unsub1 = listen('settings-updated', fetchAll);
        const unsub2 = listen<string>('proxy-account-switched', (e) => {
            setSwitchedAccount(e.payload);
            setTimeout(() => setSwitchedAccount(null), 5000);
            fetchAll();
        });
        const unsub3 = listen<string>('proxy-all-exhausted', (e) => {
            setMessage({ type: 'error', text: e.payload });
        });
        return () => {
            unsub1.then(fn => fn());
            unsub2.then(fn => fn());
            unsub3.then(fn => fn());
        };
    }, []);

    const toggleProxy = async (enabled: boolean) => {
        if (!settings) return;
        setSaving(true);
        setMessage(null);
        try {
            await invoke('update_settings', {
                settings: { ...settings, proxy_enabled: enabled, proxy_port: port },
            });
            setMessage({ type: 'success', text: enabled ? 'Proxy started' : 'Proxy stopped' });
            setTimeout(() => setMessage(null), 3000);
        } catch (e) {
            setMessage({ type: 'error', text: `Operation failed: ${e}` });
        } finally {
            setSaving(false);
        }
    };

    const savePort = async () => {
        if (!settings) return;
        setSaving(true);
        try {
            await invoke('update_settings', {
                settings: { ...settings, proxy_port: port },
            });
            setMessage({ type: 'success', text: 'Port updated (effective after proxy restart)' });
            setTimeout(() => setMessage(null), 3000);
        } catch (e) {
            setMessage({ type: 'error', text: `Save failed: ${e}` });
        } finally {
            setSaving(false);
        }
    };

    const handleSetEnv = async (enable: boolean) => {
        setEnvWriting(true);
        setMessage(null);
        try {
            const result = await invoke<string>('set_proxy_env', { port, enable });
            setMessage({ type: 'success', text: result + ' (effective in new terminal windows)' });
            setTimeout(() => setMessage(null), 5000);
        } catch (e) {
            setMessage({ type: 'error', text: `${e}` });
        } finally {
            setEnvWriting(false);
        }
    };

    const handleKill = async () => {
        setKilling(true);
        try {
            const result = await invoke<string>('kill_codex_processes');
            setMessage({ type: 'success', text: result });
            setTimeout(() => setMessage(null), 3000);
        } catch (e) {
            setMessage({ type: 'error', text: `${e}` });
        } finally {
            setKilling(false);
        }
    };

    const isRunning = status?.is_running ?? false;
    const isEnabled = settings?.proxy_enabled ?? false;

    return (
        <div className="proxy-page">
            <div className="proxy-header">
                <h2>Proxy Service</h2>
                <div className={`proxy-status-badge ${isRunning ? 'running' : 'stopped'}`}>
                    <span className="status-dot" />
                    {isRunning ? 'Running' : 'Stopped'}
                </div>
            </div>

            {message && (
                <div className={`settings-message ${message.type}`}>
                    {message.text}
                </div>
            )}

            {switchedAccount && (
                <div className="settings-message success">
                    Proxy auto-switched to account: {switchedAccount}
                </div>
            )}

            {/* Proxy Toggle */}
            <div className="settings-section">
                <h3>Proxy Control</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Local Proxy Server</span>
                        <span className="setting-desc">
                            Codex CLI connects to OpenAI via proxy, supports seamless auto-switch and 429 smart retry
                        </span>
                    </div>
                    <button
                        className={`proxy-toggle-btn ${isEnabled ? 'on' : 'off'}`}
                        onClick={() => toggleProxy(!isEnabled)}
                        disabled={saving}
                    >
                        {saving ? '...' : isEnabled ? 'Stop Proxy' : 'Start Proxy'}
                    </button>
                </div>

                <div className="setting-item sub-item">
                    <div className="setting-info">
                        <span className="setting-label">Proxy Port</span>
                    </div>
                    <div className="port-input-group">
                        <input
                            type="number"
                            className="number-input"
                            min={1024}
                            max={65535}
                            value={port}
                            onChange={e => setPort(parseInt(e.target.value) || 18080)}
                        />
                        {port !== settings?.proxy_port && (
                            <button className="btn btn-sm btn-primary" onClick={savePort} disabled={saving}>
                                <Save size={12} /> Save
                            </button>
                        )}
                    </div>
                </div>

                <div className="setting-item sub-item">
                    <div className="setting-info">
                        <span className="setting-label">Allow LAN Access</span>
                        <span className="setting-desc">Listen on 0.0.0.0, Windows machines on same LAN can connect directly to this proxy</span>
                    </div>
                    <label className="toggle">
                        <input
                            type="checkbox"
                            checked={settings?.proxy_allow_lan ?? false}
                            onChange={async e => {
                                if (!settings) return;
                                const updated = { ...settings, proxy_allow_lan: e.target.checked };
                                setSettings(updated);
                                await invoke('update_settings', { settings: updated });
                            }}
                        />
                        <span className="toggle-slider"></span>
                    </label>
                </div>
            </div>

            {/* Environment Variables */}
            <div className="settings-section">
                <h3>Environment Variables</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Manual Launch</span>
                        <span className="setting-desc">Copy command to terminal to run</span>
                    </div>
                    <button
                        className="copy-command-button"
                        onClick={() => {
                            navigator.clipboard.writeText(
                                `OPENAI_BASE_URL=${status?.base_url ?? `http://localhost:${port}/v1`} codex`
                            );
                            setCopied(true);
                            setTimeout(() => setCopied(false), 2000);
                        }}
                    >
                        <code>OPENAI_BASE_URL={status?.base_url ?? `http://localhost:${port}/v1`} codex</code>
                        {copied ? <Check size={12} /> : <Copy size={12} />}
                    </button>
                </div>

                {status?.allow_lan && status.lan_base_url && (
                    <div className="setting-item">
                        <div className="setting-info">
                            <span className="setting-label">LAN Client</span>
                            <span className="setting-desc">Windows machines can point OPENAI_BASE_URL to the address below</span>
                        </div>
                        <button
                            className="copy-command-button"
                            onClick={() => {
                                navigator.clipboard.writeText(
                                    `OPENAI_BASE_URL=${status.lan_base_url} codex`
                                );
                                setCopied(true);
                                setTimeout(() => setCopied(false), 2000);
                            }}
                        >
                            <code>OPENAI_BASE_URL={status.lan_base_url} codex</code>
                            {copied ? <Check size={12} /> : <Copy size={12} />}
                        </button>
                    </div>
                )}

                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Global Proxy (CLI + App Full Coverage)</span>
                        <span className="setting-desc">
                            Write to ~/.zshrc, launchctl and ~/.codex/config.toml, both terminal CLI and Codex App use proxy
                        </span>
                    </div>
                    <div className="env-btn-group">
                        <button
                            className="btn btn-sm btn-primary"
                            onClick={() => handleSetEnv(true)}
                            disabled={envWriting}
                        >
                            {envWriting ? '...' : 'Write Env Variable'}
                        </button>
                        <button
                            className="btn btn-sm btn-ghost"
                            onClick={() => handleSetEnv(false)}
                            disabled={envWriting}
                        >
                            Remove
                        </button>
                    </div>
                </div>
            </div>

            {/* Scheduled Quota Refresh */}
            <div className="settings-section">
                <h3>Scheduled Quota Refresh</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Auto Refresh Account Quota</span>
                        <span className="setting-desc">Sort by last update time, auto cycle refresh all account quota data</span>
                    </div>
                    <label className="toggle">
                        <input
                            type="checkbox"
                            checked={settings?.quota_refresh_enabled ?? false}
                            onChange={async e => {
                                if (!settings) return;
                                const updated = { ...settings, quota_refresh_enabled: e.target.checked };
                                setSettings(updated);
                                await invoke('update_settings', { settings: updated });
                            }}
                        />
                        <span className="toggle-slider"></span>
                    </label>
                </div>
                {settings?.quota_refresh_enabled && (
                    <>
                        <div className="setting-item sub-item">
                            <div className="setting-info">
                                <span className="setting-label">Refresh Interval (minutes/account)</span>
                                <span className="setting-desc">Interval between each account refresh</span>
                            </div>
                            <input
                                type="number"
                                className="number-input"
                                min={1}
                                max={60}
                                value={settings.quota_refresh_interval}
                                onChange={async e => {
                                    const val = parseInt(e.target.value) || 5;
                                    const updated = { ...settings, quota_refresh_interval: val };
                                    setSettings(updated);
                                    await invoke('update_settings', { settings: updated });
                                }}
                            />
                        </div>
                        <div className="setting-item sub-item">
                            <div className="setting-info">
                                <span className="setting-label">Accounts per Refresh Round</span>
                                <span className="setting-desc">How many accounts to refresh per cycle</span>
                            </div>
                            <input
                                type="number"
                                className="number-input"
                                min={1}
                                max={10}
                                value={settings.quota_refresh_batch}
                                onChange={async e => {
                                    const val = parseInt(e.target.value) || 1;
                                    const updated = { ...settings, quota_refresh_batch: val };
                                    setSettings(updated);
                                    await invoke('update_settings', { settings: updated });
                                }}
                            />
                        </div>
                    </>
                )}
            </div>

            {/* Notification Settings */}
            <div className="settings-section">
                <h3>Switch Notifications</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">macOS System Notification</span>
                        <span className="setting-desc">Show system notification at top-right corner when switching accounts</span>
                    </div>
                    <label className="toggle">
                        <input
                            type="checkbox"
                            checked={settings?.notify_on_switch ?? false}
                            onChange={async e => {
                                if (!settings) return;
                                const updated = { ...settings, notify_on_switch: e.target.checked };
                                setSettings(updated);
                                await invoke('update_settings', { settings: updated });
                            }}
                        />
                        <span className="toggle-slider"></span>
                    </label>
                </div>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Inject Conversation Notification (Experimental)</span>
                        <span className="setting-desc">Insert a switch notification message in Codex conversation after switching. May affect conversation state.</span>
                    </div>
                    <label className="toggle">
                        <input
                            type="checkbox"
                            checked={settings?.inject_switch_message ?? false}
                            onChange={async e => {
                                if (!settings) return;
                                const updated = { ...settings, inject_switch_message: e.target.checked };
                                setSettings(updated);
                                await invoke('update_settings', { settings: updated });
                            }}
                        />
                        <span className="toggle-slider"></span>
                    </label>
                </div>
            </div>

            {/* Codex Config */}
            <div className="settings-section">
                <h3>Codex Config</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Fast Mode</span>
                        <span className="setting-desc">
                            Faster inference speed, but consumes 2x quota. {fastMode ? 'Current: Enabled' : 'Current: Disabled'}
                        </span>
                    </div>
                    <button
                        className={`proxy-toggle-btn ${fastMode ? 'on' : 'off'}`}
                        onClick={async () => {
                            try {
                                const result = await invoke<string>('set_codex_fast_mode', { enable: !fastMode });
                                setFastMode(!fastMode);
                                setMessage({ type: 'success', text: result });
                                setTimeout(() => setMessage(null), 3000);
                            } catch (e) {
                                setMessage({ type: 'error', text: `${e}` });
                            }
                        }}
                    >
                        {fastMode ? 'Disable Fast' : 'Enable Fast'}
                    </button>
                </div>
            </div>

            {/* Process Management */}
            <div className="settings-section">
                <h3>Process Management</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Kill All Codex Processes</span>
                        <span className="setting-desc">
                            Force kill all running codex processes, used for restarting after proxy mode switch or troubleshooting
                        </span>
                    </div>
                    <button
                        className="action-button warning"
                        onClick={handleKill}
                        disabled={killing}
                    >
                        {killing ? 'Killing...' : 'Kill Processes'}
                    </button>
                </div>
            </div>

            {/* Smart Switch Strategy */}
            <div className="settings-section">
                <h3>Smart Switch Strategy</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">5h Quota Preventive Switch Threshold</span>
                        <span className="setting-desc">Preemptively switch when remaining quota falls below this percentage (0 = only triggered by 429, recommended 10)</span>
                    </div>
                    <div className="threshold-input-group">
                        <input
                            type="number"
                            className="number-input"
                            min={0}
                            max={50}
                            value={settings?.proxy_threshold_5h ?? 0}
                            onChange={e => {
                                if (!settings) return;
                                const val = parseInt(e.target.value) || 0;
                                const updated = { ...settings, proxy_threshold_5h: val };
                                setSettings(updated);
                                invoke('update_settings', { settings: updated });
                            }}
                        />
                        <span className="threshold-unit">%</span>
                    </div>
                </div>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Weekly Quota Preventive Switch Threshold</span>
                        <span className="setting-desc">Preemptively switch when remaining weekly quota falls below this percentage (0 = only triggered by 429, recommended 5)</span>
                    </div>
                    <div className="threshold-input-group">
                        <input
                            type="number"
                            className="number-input"
                            min={0}
                            max={50}
                            value={settings?.proxy_threshold_weekly ?? 0}
                            onChange={e => {
                                if (!settings) return;
                                const val = parseInt(e.target.value) || 0;
                                const updated = { ...settings, proxy_threshold_weekly: val };
                                setSettings(updated);
                                invoke('update_settings', { settings: updated });
                            }}
                        />
                        <span className="threshold-unit">%</span>
                    </div>
                </div>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Free Account Guard Line</span>
                        <span className="setting-desc">Force switch when Free account remaining quota falls below this percentage (0 = no special handling, recommended 35)</span>
                    </div>
                    <div className="threshold-input-group">
                        <input
                            type="number"
                            className="number-input"
                            min={0}
                            max={80}
                            value={settings?.proxy_free_guard ?? 0}
                            onChange={e => {
                                if (!settings) return;
                                const val = parseInt(e.target.value) || 0;
                                const updated = { ...settings, proxy_free_guard: val };
                                setSettings(updated);
                                invoke('update_settings', { settings: updated });
                            }}
                        />
                        <span className="threshold-unit">%</span>
                    </div>
                </div>
            </div>

        </div>
    );
}
