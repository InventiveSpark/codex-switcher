import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import Markdown from 'react-markdown';
import './Skills.css';

interface SkillApps {
    codex: boolean;
    claude: boolean;
    gemini: boolean;
    opencode: boolean;
}

interface InstalledSkill {
    id: string;
    name: string;
    description: string;
    directory: string;
    source: string;
    repo_owner: string | null;
    repo_name: string | null;
    apps: SkillApps;
    installed_at: string;
}

interface DiscoverableSkill {
    key: string;
    name: string;
    description: string;
    directory: string;
    repo_owner: string;
    repo_name: string;
    repo_branch: string;
    installed: boolean;
}

interface SkillRepo {
    owner: string;
    name: string;
    branch: string;
    enabled: boolean;
}

type Tab = 'installed' | 'discover' | 'repos';

const APPS = ['codex', 'claude', 'gemini', 'opencode'] as const;

export function Skills() {
    const [tab, setTab] = useState<Tab>('installed');
    const [installed, setInstalled] = useState<InstalledSkill[]>([]);
    const [discovered, setDiscovered] = useState<DiscoverableSkill[]>([]);
    const [repos, setRepos] = useState<SkillRepo[]>([]);
    const [appStatus, setAppStatus] = useState<Record<string, boolean>>({});
    const [search, setSearch] = useState('');
    const [loading, setLoading] = useState(false);
    const [message, setMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);
    const [detailSkill, setDetailSkill] = useState<InstalledSkill | null>(null);
    const [detailContent, setDetailContent] = useState<string>('');
    const [confirmDelete, setConfirmDelete] = useState<{ id: string; name: string } | null>(null);
    const [confirmInput, setConfirmInput] = useState('');

    // New repo form
    const [newOwner, setNewOwner] = useState('');
    const [newName, setNewName] = useState('');
    const [newBranch, setNewBranch] = useState('main');

    const showMsg = (type: 'success' | 'error', text: string) => {
        setMessage({ type, text });
        setTimeout(() => setMessage(null), 5000);
    };

    const loadInstalled = async (rescan = false) => {
        try {
            if (rescan) {
                const count = await invoke<number>('scan_and_import_skills');
                if (count > 0) {
                    showMsg('success', `Auto imported ${count} new skills`);
                }
            }
            const list = await invoke<InstalledSkill[]>('get_installed_skills');
            setInstalled(list);
        } catch (e) { console.error(e); }
    };

    const loadRepos = async () => {
        try {
            const list = await invoke<SkillRepo[]>('get_skill_repos');
            setRepos(list);
        } catch (e) { console.error(e); }
    };

    const loadAppStatus = async () => {
        try {
            const status = await invoke<Record<string, boolean>>('get_skill_app_status');
            setAppStatus(status);
        } catch (e) { console.error(e); }
    };

    useEffect(() => {
        loadInstalled(true); // Scan and import new skills on first load
        loadRepos();
        loadAppStatus();
    }, []);

    const handleDiscover = async () => {
        setLoading(true);
        try {
            const list = await invoke<DiscoverableSkill[]>('discover_skills');
            setDiscovered(list);
            showMsg('success', `Discovered ${list.length} skills`);
        } catch (e) {
            showMsg('error', `Discovery failed: ${e}`);
        } finally {
            setLoading(false);
        }
    };

    const handleInstall = async (skill: DiscoverableSkill) => {
        setLoading(true);
        try {
            await invoke('install_skill', { skillJson: JSON.stringify(skill) });
            showMsg('success', `Installed ${skill.name}`);
            await loadInstalled();
            // Mark as installed
            setDiscovered(prev => prev.map(s => s.key === skill.key ? { ...s, installed: true } : s));
        } catch (e) {
            showMsg('error', `Install failed: ${e}`);
        } finally {
            setLoading(false);
        }
    };

    const handleUninstall = (id: string, name: string) => {
        setConfirmDelete({ id, name });
        setConfirmInput('');
    };

    const executeUninstall = async () => {
        if (!confirmDelete) return;
        try {
            await invoke('uninstall_skill', { skillId: confirmDelete.id });
            showMsg('success', `Uninstalled ${confirmDelete.name}`);
            setConfirmDelete(null);
            setConfirmInput('');
            await loadInstalled();
        } catch (e) {
            showMsg('error', `Uninstall failed: ${e}`);
        }
    };

    const handleOpenDetail = async (skill: InstalledSkill) => {
        setDetailSkill(skill);
        try {
            const content = await invoke<string>('get_skill_content', { directory: skill.directory });
            setDetailContent(content);
        } catch {
            setDetailContent('Cannot read SKILL.md');
        }
    };

    const handleToggleAppLink = async (app: string, enabled: boolean) => {
        try {
            await invoke('toggle_skill_app_link', { app, enabled });
            showMsg('success', enabled ? `${app} linked to Skills` : `${app} unlinked from Skills`);
            await loadAppStatus();
        } catch (e) {
            showMsg('error', `Operation failed: ${e}`);
        }
    };

    const handleAddRepo = async () => {
        if (!newOwner || !newName) return;
        try {
            await invoke('add_skill_repo', { owner: newOwner, name: newName, branch: newBranch });
            showMsg('success', `Added ${newOwner}/${newName}`);
            setNewOwner('');
            setNewName('');
            setNewBranch('main');
            await loadRepos();
        } catch (e) {
            showMsg('error', `${e}`);
        }
    };

    const handleRemoveRepo = async (owner: string, name: string) => {
        try {
            await invoke('remove_skill_repo', { owner, name });
            await loadRepos();
        } catch (e) {
            showMsg('error', `${e}`);
        }
    };

    const handleSyncAll = async () => {
        try {
            await invoke('sync_all_skills');
            showMsg('success', 'Sync complete');
        } catch (e) {
            showMsg('error', `Sync failed: ${e}`);
        }
    };

    const filtered = installed.filter(s =>
        s.name.toLowerCase().includes(search.toLowerCase()) ||
        s.description.toLowerCase().includes(search.toLowerCase())
    );

    const filteredDiscover = discovered.filter(s =>
        s.name.toLowerCase().includes(search.toLowerCase()) ||
        s.description.toLowerCase().includes(search.toLowerCase())
    );

    return (
        <div className="skills-page">
            <div className="skills-header">
                <h2>Skills</h2>
                <div className="skills-tabs">
                    <button className={`tab-btn ${tab === 'installed' ? 'active' : ''}`} onClick={() => { setTab('installed'); loadInstalled(true); }}>
                        Installed ({installed.length})
                    </button>
                    <button className={`tab-btn ${tab === 'discover' ? 'active' : ''}`} onClick={() => { setTab('discover'); if (discovered.length === 0) handleDiscover(); }}>
                        Discover
                    </button>
                    <button className={`tab-btn ${tab === 'repos' ? 'active' : ''}`} onClick={() => setTab('repos')}>
                        Repos
                    </button>
                </div>
            </div>

            {message && (
                <div className={`settings-message ${message.type}`}>{message.text}</div>
            )}

            <div className="skills-search">
                <input
                    type="text"
                    placeholder="Search skills..."
                    value={search}
                    onChange={e => setSearch(e.target.value)}
                    className="search-input"
                />
                {tab === 'installed' && (
                    <button className="btn btn-sm btn-ghost" onClick={handleSyncAll}>Sync All</button>
                )}
                {tab === 'discover' && (
                    <button className="btn btn-sm btn-primary" onClick={handleDiscover} disabled={loading}>
                        {loading ? 'Scanning...' : 'Refresh'}
                    </button>
                )}
            </div>

            {/* Installed List */}
            {tab === 'installed' && (
                <>
                    {/* CLI Sync Status */}
                    <div className="app-sync-bar">
                        {APPS.map(app => (
                            <label key={app} className={`app-sync-item ${appStatus[app] ? 'linked' : ''}`}>
                                <input
                                    type="checkbox"
                                    checked={appStatus[app] || false}
                                    onChange={e => handleToggleAppLink(app, e.target.checked)}
                                />
                                <span>{app}</span>
                                <span className="link-status">{appStatus[app] ? 'Linked' : 'Unlinked'}</span>
                            </label>
                        ))}
                    </div>

                    <div className="skills-list">
                        {filtered.length === 0 ? (
                            <div className="skills-empty">No installed skills</div>
                        ) : filtered.map(skill => (
                            <div key={skill.id} className="skill-card" onClick={() => handleOpenDetail(skill)} style={{ cursor: 'pointer' }}>
                                <div className="skill-info">
                                    <div className="skill-name">{skill.name}</div>
                                    <div className="skill-desc">{skill.description || 'No description'}</div>
                                    <div className="skill-meta">
                                        {skill.source === 'github' && skill.repo_owner && (
                                            <span className="skill-source">{skill.repo_owner}/{skill.repo_name}</span>
                                        )}
                                        {skill.source === 'local' && <span className="skill-source">Local</span>}
                                    </div>
                                </div>
                                <button
                                    className="btn btn-sm btn-danger"
                                    onClick={(e) => { e.stopPropagation(); handleUninstall(skill.id, skill.name); }}
                                    title="Uninstall"
                                >
                                    Delete
                                </button>
                            </div>
                        ))}
                    </div>
                </>
            )}

            {/* Discover List */}
            {tab === 'discover' && (
                <div className="skills-list">
                    {loading && <div className="skills-empty">Scanning GitHub repositories...</div>}
                    {!loading && filteredDiscover.length === 0 && (
                        <div className="skills-empty">Click "Refresh" to discover skills from repositories</div>
                    )}
                    {filteredDiscover.map(skill => (
                        <div key={skill.key} className="skill-card">
                            <div className="skill-info">
                                <div className="skill-name">{skill.name}</div>
                                <div className="skill-desc">{skill.description || 'No description'}</div>
                                <div className="skill-meta">
                                    <span className="skill-source">{skill.repo_owner}/{skill.repo_name}</span>
                                </div>
                            </div>
                            {skill.installed ? (
                                <span className="skill-installed-badge">Installed</span>
                            ) : (
                                <button
                                    className="btn btn-sm btn-primary"
                                    onClick={() => handleInstall(skill)}
                                    disabled={loading}
                                >
                                    Install
                                </button>
                            )}
                        </div>
                    ))}
                </div>
            )}

            {/* Repo Management */}
            {tab === 'repos' && (
                <div className="repos-section">
                    <div className="repo-list">
                        {repos.map(repo => (
                            <div key={`${repo.owner}/${repo.name}`} className="repo-item">
                                <div className="repo-info">
                                    <span className="repo-name">{repo.owner}/{repo.name}</span>
                                    <span className="repo-branch">{repo.branch}</span>
                                </div>
                                <button
                                    className="btn btn-sm btn-danger"
                                    onClick={() => handleRemoveRepo(repo.owner, repo.name)}
                                >
                                    Remove
                                </button>
                            </div>
                        ))}
                    </div>
                    <div className="repo-add">
                        <input placeholder="owner" value={newOwner} onChange={e => setNewOwner(e.target.value)} className="repo-input" />
                        <span>/</span>
                        <input placeholder="repo" value={newName} onChange={e => setNewName(e.target.value)} className="repo-input" />
                        <input placeholder="branch" value={newBranch} onChange={e => setNewBranch(e.target.value)} className="repo-input small" />
                        <button className="btn btn-sm btn-primary" onClick={handleAddRepo}>Add</button>
                    </div>
                </div>
            )}
            {/* Delete Confirm Modal */}
            {confirmDelete && (
                <div className="skill-detail-overlay" onClick={() => setConfirmDelete(null)}>
                    <div className="skill-detail-modal confirm-delete-modal" onClick={e => e.stopPropagation()}>
                        <div className="detail-header">
                            <h2>Confirm Uninstall</h2>
                            <button className="detail-close" onClick={() => setConfirmDelete(null)}>✕</button>
                        </div>
                        <div className="detail-content">
                            <p>About to uninstall <strong>{confirmDelete.name}</strong>, this will remove the skill from all CLI directories.</p>
                            <p style={{ marginTop: '12px', color: 'var(--text-secondary)' }}>
                                Please enter skill name <code>{confirmDelete.name}</code> to confirm:
                            </p>
                            <input
                                type="text"
                                className="search-input"
                                style={{ marginTop: '8px' }}
                                placeholder={confirmDelete.name}
                                value={confirmInput}
                                onChange={e => setConfirmInput(e.target.value)}
                                autoFocus
                                onKeyDown={e => {
                                    if (e.key === 'Enter' && confirmInput === confirmDelete.name) {
                                        executeUninstall();
                                    }
                                }}
                            />
                        </div>
                        <div className="detail-footer">
                            <button className="btn btn-sm btn-ghost" onClick={() => setConfirmDelete(null)}>Cancel</button>
                            <button
                                className="btn btn-sm btn-danger"
                                disabled={confirmInput !== confirmDelete.name}
                                onClick={executeUninstall}
                            >
                                Confirm Uninstall
                            </button>
                        </div>
                    </div>
                </div>
            )}

            {/* Skill Detail Modal */}
            {detailSkill && (
                <div className="skill-detail-overlay" onClick={() => setDetailSkill(null)}>
                    <div className="skill-detail-modal" onClick={e => e.stopPropagation()}>
                        <div className="detail-header">
                            <div>
                                <h2>{detailSkill.name}</h2>
                                <p className="detail-desc">{detailSkill.description}</p>
                            </div>
                            <button className="detail-close" onClick={() => setDetailSkill(null)}>✕</button>
                        </div>
                        <div className="detail-content">
                            <Markdown>{detailContent}</Markdown>
                        </div>
                        <div className="detail-footer">
                            <button
                                className="btn btn-sm btn-danger"
                                onClick={() => { handleUninstall(detailSkill.id, detailSkill.name); setDetailSkill(null); }}
                            >
                                Uninstall
                            </button>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
}
