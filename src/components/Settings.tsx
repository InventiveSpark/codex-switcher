import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Palette, Server, Monitor, Wrench, Save, Github } from 'lucide-react';
import './Settings.css';

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
}

const IDE_OPTIONS = [
    { value: 'Windsurf', label: 'Windsurf' },
    { value: 'Antigravity', label: 'Antigravity' },
    { value: 'Cursor', label: 'Cursor' },
    { value: 'VSCode', label: 'VS Code' },
    { value: 'Codex', label: 'Codex App' },
];

export function Settings() {
    const [settings, setSettings] = useState<AppSettings>({
        auto_reload_ide: false,
        primary_ide: 'Windsurf',
        use_pkill_restart: false,
        background_refresh: false,
        refresh_interval_minutes: 30,
        inactive_refresh_days: 7,
        theme_palette: 'midnight',
        allow_auto_switch_to_free: false,
        proxy_enabled: false,
        proxy_port: 18080,
        proxy_allow_lan: false,
    });
    const [saving, setSaving] = useState(false);
    const [repairing, setRepairing] = useState(false);
    const [message, setMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);

    useEffect(() => {
        loadSettings();
    }, []);

    const loadSettings = async () => {
        try {
            const data = await invoke<AppSettings>('get_settings');
            setSettings(data);
        } catch (e) {
            console.error('Failed to load settings:', e);
        }
    };

    const saveSettings = async () => {
        setSaving(true);
        setMessage(null);
        try {
            await invoke('update_settings', { settings });
            setMessage({ type: 'success', text: '✅ Settings saved' });
            setTimeout(() => setMessage(null), 3000);
        } catch (e) {
            setMessage({ type: 'error', text: `❌ Save failed: ${e}` });
        } finally {
            setSaving(false);
        }
    };

    const updateField = <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => {
        setSettings(prev => ({ ...prev, [key]: value }));
    };

    const handleRepair = async () => {
        if (!confirm('This will attempt to remove Codex App quarantine attributes.\n\nSystem may prompt for password to gain permissions. Continue?')) {
            return;
        }

        setRepairing(true);
        setMessage(null);
        try {
            const ticket = await invoke<string>('request_quarantine_fix_ticket');
            await invoke('fix_codex_quarantine', { ticket });
            alert('✅ Fix successful!\n\nPlease try reopening Codex App now.');
        } catch (e) {
            alert(`❌ Fix failed: ${e}`);
        } finally {
            setRepairing(false);
        }
    };


    return (
        <div className="settings-page">
            <div className="settings-header">
                <h2>Settings</h2>
                <button
                    className="save-button"
                    onClick={saveSettings}
                    disabled={saving}
                >
                    <Save size={14} />
                    {saving ? 'Saving...' : 'Save Settings'}
                </button>
            </div>

            {message && (
                <div className={`settings-message ${message.type}`}>
                    {message.text}
                </div>
            )}

            <div className="settings-section">
                <h3><Palette size={16} /> Appearance</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Theme</span>
                        <span className="setting-desc">Main interface color tone style</span>
                    </div>
                    <select
                        className="select-input"
                        value={settings.theme_palette}
                        onChange={e => updateField('theme_palette', e.target.value)}
                    >
                        <option value="midnight">Midnight Dark</option>
                        <option value="github">Classic Blue</option>
                        <option value="agate">Agate Green</option>
                    </select>
                </div>
            </div>

            <div className="settings-section">
                <h3><Server size={16} /> Background Service</h3>

                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Background Keepalive & Sync</span>
                        <span className="setting-desc">Current account only authoritative sync; inactive accounts keepalive refresh by exclusive strategy (effective after save)</span>
                    </div>
                    <label className="toggle">
                        <input
                            type="checkbox"
                            checked={settings.background_refresh}
                            onChange={e => updateField('background_refresh', e.target.checked)}
                        />
                        <span className="toggle-slider"></span>
                        <span className={`toggle-text ${settings.background_refresh ? 'on' : ''}`}>
                            {settings.background_refresh ? 'Enabled' : 'Disabled'}
                        </span>
                    </label>
                </div>

                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Smart Switch Allow FREE Account</span>
                        <span className="setting-desc">When clicking "Switch Next Account", whether to allow auto finding and switching to FREE account (default prioritizes paid accounts)</span>
                    </div>
                    <label className="toggle">
                        <input
                            type="checkbox"
                            checked={settings.allow_auto_switch_to_free}
                            onChange={e => updateField('allow_auto_switch_to_free', e.target.checked)}
                        />
                        <span className="toggle-slider"></span>
                    </label>
                </div>

                {
                    settings.background_refresh && (
                        <>
                            <div className="setting-item sub-item">
                                <div className="setting-info">
                                    <span className="setting-label">Schedule Interval (minutes)</span>
                                </div>
                                <input
                                    type="number"
                                    className="number-input"
                                    min={5}
                                    max={120}
                                    value={settings.refresh_interval_minutes}
                                    onChange={e => updateField('refresh_interval_minutes', parseInt(e.target.value) || 30)}
                                />
                            </div>
                            <div className="setting-item sub-item">
                                <div className="setting-info">
                                    <span className="setting-label">Inactive Keepalive Threshold (days)</span>
                                    <span className="setting-desc">Scheduler only attempts keepalive refresh when account last_refresh exceeds this threshold</span>
                                </div>
                                <input
                                    type="number"
                                    className="number-input"
                                    min={1}
                                    max={30}
                                    value={settings.inactive_refresh_days}
                                    onChange={e => updateField('inactive_refresh_days', parseInt(e.target.value) || 7)}
                                />
                            </div>
                        </>
                    )
                }
            </div >

            <div className="settings-section">
                <h3><Monitor size={16} /> IDE Reload</h3>

                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Auto Reload IDE</span>
                        <span className="setting-desc">Auto reload IDE after switching account to apply new token</span>
                    </div>
                    <label className="toggle">
                        <input
                            type="checkbox"
                            checked={settings.auto_reload_ide}
                            onChange={e => updateField('auto_reload_ide', e.target.checked)}
                        />
                        <span className="toggle-slider"></span>
                    </label>
                </div>

                {settings.auto_reload_ide && (
                    <>
                        <div className="setting-item sub-item">
                            <div className="setting-info">
                                <span className="setting-label">Primary IDE</span>
                                <span className="setting-desc">Only reload selected IDE</span>
                            </div>
                            <select
                                className="select-input"
                                value={settings.primary_ide}
                                onChange={e => updateField('primary_ide', e.target.value)}
                            >
                                {IDE_OPTIONS.map(opt => (
                                    <option key={opt.value} value={opt.value}>{opt.label}</option>
                                ))}
                            </select>
                        </div>

                        <div className="setting-item sub-item">
                            <div className="setting-info">
                                <span className="setting-label">Use Kill Process Restart</span>
                                <span className="setting-desc">Use pkill restart (recommended for Windsurf, no permissions needed)</span>
                            </div>
                            <label className="toggle">
                                <input
                                    type="checkbox"
                                    checked={settings.use_pkill_restart}
                                    onChange={e => updateField('use_pkill_restart', e.target.checked)}
                                />
                                <span className="toggle-slider"></span>
                            </label>
                        </div>
                    </>
                )}
            </div>

            <div className="settings-section danger">
                <h3><Wrench size={16} /> Troubleshooting</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Fix Codex App Crash</span>
                        <span className="setting-desc">Remove macOS quarantine attribute (requires admin privileges)</span>
                    </div>
                    <button
                        className="action-button warning"
                        onClick={handleRepair}
                        disabled={repairing}
                    >
                        {repairing ? 'Fixing...' : 'Fix Now'}
                    </button>
                </div>
            </div>

            <div className="settings-section">
                <h3><Github size={16} /> About</h3>
                <div className="setting-item">
                    <div className="setting-info">
                        <span className="setting-label">Codex Switcher</span>
                        <span className="setting-desc">Multi-account smart switching + local proxy + usage stats</span>
                    </div>
                    <a
                        className="action-button github-link"
                        href="https://github.com/xtftbwvfp/codex-switcher"
                        target="_blank"
                        rel="noopener noreferrer"
                    >
                        <Github size={14} /> GitHub
                    </a>
                </div>
            </div>
        </div >
    );
}
