import { useState, useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useAccounts } from '../hooks/useAccounts';
import './AddAccountModal.css';

interface AddAccountModalProps {
    isOpen: boolean;
    onClose: () => void;
    onAdd: (name: string, notes?: string) => Promise<void>;
    onSuccess?: () => void;  // Callback after successful addition, used to refresh parent list
}

type TabType = 'official' | 'openai';

export function AddAccountModal({ isOpen, onClose, onAdd, onSuccess }: AddAccountModalProps) {
    const { startOAuthLogin, finalizeOAuthLogin } = useAccounts();
    const [activeTab, setActiveTab] = useState<TabType>('openai');
    const [name, setName] = useState('');
    const [notes, setNotes] = useState('');
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [oauthStatus, setOauthStatus] = useState<string>('');

    // Listen for authorization code from backend
    useEffect(() => {
        if (!isOpen) return;

        const unlisten = listen<string>('oauth-callback-received', async (event) => {
            const code = event.payload;
            setOauthStatus('Authorization code received, exchanging for token...');
            try {
                await finalizeOAuthLogin(code);
                setOauthStatus('Authorization successful! Account added.');
                setLoading(false);
                // Delay closing modal to show success message
                setTimeout(() => {
                    onSuccess?.();  // Notify parent to refresh list
                    onClose();
                }, 1000);
            } catch (err) {
                setError(String(err));
                setOauthStatus('');
                setLoading(false);
            }
        });

        return () => {
            unlisten.then(f => f());
        };
    }, [isOpen, finalizeOAuthLogin]);

    if (!isOpen) return null;

    // Handle official import
    const handleSubmitOfficial = async (e: React.FormEvent) => {
        e.preventDefault();
        if (!name.trim()) {
            setError('Please enter account name');
            return;
        }

        setLoading(true);
        setError(null);

        try {
            await onAdd(name.trim(), notes.trim() || undefined);
            handleClose();
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    // Handle OpenAI login
    const handleOpenAILogin = async () => {
        setLoading(true);
        setError(null);
        setOauthStatus('Starting official browser authorization...');

        try {
            // Start OAuth backend task, backend will open browser and start listener
            await startOAuthLogin();
            setOauthStatus('Please complete OpenAI authorization in the opened browser window...');
        } catch (err) {
            setError(String(err));
            setOauthStatus('');
            setLoading(false);
        }
    };

    const handleClose = () => {
        if (loading && !oauthStatus.includes('successful')) return;
        setName('');
        setNotes('');
        setError(null);
        setOauthStatus('');
        onClose();
    };

    return (
        <div className="modal-overlay" onClick={handleClose}>
            <div className="modal-content" onClick={e => e.stopPropagation()}>
                <div className="modal-header">
                    <div className="header-top">
                        <h2>Add Account</h2>
                        <button className="close-btn" onClick={handleClose} disabled={loading && !oauthStatus.includes('successful')}>
                            ×
                        </button>
                    </div>
                    <div className="modal-tabs">
                        <button
                            className={`tab-item ${activeTab === 'openai' ? 'active' : ''}`}
                            onClick={() => !loading && setActiveTab('openai')}
                        >
                            OpenAI Login (Recommended)
                        </button>
                        <button
                            className={`tab-item ${activeTab === 'official' ? 'active' : ''}`}
                            onClick={() => !loading && setActiveTab('official')}
                        >
                            Import from Official
                        </button>
                    </div>
                </div>

                <div className="modal-body">
                    {activeTab === 'official' ? (
                        <form onSubmit={handleSubmitOfficial}>
                            <p className="modal-tip">
                                Will extract authentication information from local official Codex login status (`auth.json`).
                            </p>

                            <div className="form-group">
                                <label htmlFor="name">Account Name *</label>
                                <input
                                    id="name"
                                    type="text"
                                    value={name}
                                    onChange={e => setName(e.target.value)}
                                    placeholder="e.g. Work Account, Personal Account"
                                    disabled={loading}
                                    autoFocus
                                />
                            </div>

                            <div className="form-group">
                                <label htmlFor="notes">Notes</label>
                                <textarea
                                    id="notes"
                                    value={notes}
                                    onChange={e => setNotes(e.target.value)}
                                    placeholder="Optional notes..."
                                    disabled={loading}
                                    rows={3}
                                />
                            </div>

                            {error && <div className="error-message">{error}</div>}

                            <div className="modal-footer" style={{ padding: '16px 0 0', border: 'none' }}>
                                <button type="button" className="btn btn-ghost" onClick={handleClose} disabled={loading}>
                                    Cancel
                                </button>
                                <button type="submit" className="btn btn-primary" disabled={loading}>
                                    {loading ? 'Importing...' : 'Import Current Account'}
                                </button>
                            </div>
                        </form>
                    ) : (
                        <div className="oauth-content">
                            <div className="oauth-icon">🛡️</div>
                            <h3 style={{ marginBottom: '8px', color: 'var(--text-primary)' }}>Official OAuth Authorization</h3>
                            <p className="oauth-desc">
                                Log in directly through OpenAI official channel. Supports token automatic renewal, multi-account switching is more stable, no need to manually update `auth.json`.
                            </p>

                            <button
                                className="btn btn-primary btn-full"
                                style={{ padding: '14px' }}
                                onClick={handleOpenAILogin}
                                disabled={loading}
                            >
                                {loading && oauthStatus ? 'Processing...' : 'Login with OpenAI'}
                            </button>

                            {!loading && (
                                <button
                                    className="btn btn-ghost btn-full"
                                    style={{ marginTop: '12px' }}
                                    onClick={handleClose}
                                >
                                    Cancel
                                </button>
                            )}

                            {oauthStatus && <div className="oauth-status">{oauthStatus}</div>}
                            {error && <div className="error-message" style={{ marginTop: '16px' }}>{error}</div>}

                            <div style={{ marginTop: '16px', fontSize: '12px', color: 'var(--text-tertiary)', textAlign: 'center' }}>
                                Authorization will complete in your system default browser, safe and trusted.
                            </div>
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
}
