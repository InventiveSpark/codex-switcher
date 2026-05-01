import { useState, useEffect } from 'react';
import { Zap } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { save } from '@tauri-apps/plugin-dialog';
import { writeTextFile } from '@tauri-apps/plugin-fs';
import { useAccounts } from './hooks/useAccounts';
import { useUsage } from './hooks/useUsage';
import { AddAccountModal } from './components/AddAccountModal';
import { Dashboard } from './components/Dashboard';
import { AccountList } from './components/AccountList';
import { Settings } from './components/Settings';
import { Proxy } from './components/Proxy';
import { Stats } from './components/Stats';
import { Skills } from './components/Skills';
import { ConfirmModal } from './components/ConfirmModal';
import './App.css';

type PageType = 'dashboard' | 'accounts' | 'proxy' | 'stats' | 'skills' | 'settings';

function App() {
  const {
    accounts,
    currentId,
    settings,
    loading,
    error,
    refresh,
    importCurrent,
    switchTo,
    deleteAccount,
    setInactiveRefreshEnabled,
    exportAccounts,
    reloadIdeWindows,
    updateSettings,
    checkSyncConflict,
    getSyncStatus,
    syncActiveWithDisk,
  } = useAccounts();

  const {
    usage,
    loading: usageLoading,
    error: usageError,
    refresh: refreshUsage,
  } = useUsage();

  const [currentPage, setCurrentPage] = useState<PageType>(() => {
    const saved = localStorage.getItem('currentPage');
    return (saved as PageType) || 'dashboard';
  });

  // Persist current tab
  useEffect(() => {
    localStorage.setItem('currentPage', currentPage);
  }, [currentPage]);
  const [showAddModal, setShowAddModal] = useState(false);
  const [schedulerError, setSchedulerError] = useState<string | null>(null);

  // Conflict confirmation modal state
  const [showConflictModal, setShowConflictModal] = useState(false);
  const [conflictAccountName, setConflictAccountName] = useState('');
  const [pendingSwitchId, setPendingSwitchId] = useState<string | null>(null);
  const [isSwitching, setIsSwitching] = useState(false);
  const [syncStatus, setSyncStatus] = useState<any>(null);
  const [proxyRunning, setProxyRunning] = useState(false);

  const checkProxyStatus = async () => {
    try {
      const s = await invoke<{ is_running: boolean }>('get_proxy_status');
      setProxyRunning(s.is_running);
    } catch { setProxyRunning(false); }
  };

  const checkSyncStatus = async () => {
    try {
      const status = await getSyncStatus();
      setSyncStatus(status);
    } catch (err) {
      console.error('Sync status check failed:', err);
    }
  };

  useEffect(() => {
    checkSyncStatus();
    checkProxyStatus();
  }, []);

  const currentAccount = accounts.find(a => a.id === currentId) || null;

  const classifyRefreshFailure = (reason: string): 'permanent' | 'transient' => {
    const lower = reason.toLowerCase();
    if (
      lower.includes('refresh_token_reused') ||
      lower.includes('refresh_token_invalidated') ||
      lower.includes('refresh_token_expired')
    ) {
      return 'permanent';
    }
    return 'transient';
  };

  // Listen for background scheduler account update events
  useEffect(() => {
    const unlisten = listen('accounts-updated', () => {
      console.log('[Frontend] Received background refresh notification, reloading account list');
      refresh();
    });

    return () => {
      unlisten.then(f => f());
    };
  }, [refresh]);

  // Listen for background refresh failure events
  useEffect(() => {
    const unlisten = listen<{ account_name: string; reason: string }>('token-refresh-failed', (event) => {
      const { account_name, reason } = event.payload;
      const timestamp = new Date().toLocaleTimeString();
      const kind = classifyRefreshFailure(reason);
      if (kind === 'permanent') {
        setSchedulerError(`Background keepalive disabled (${account_name}, re-login required) @ ${timestamp}`);
      } else {
        setSchedulerError(`Background keepalive temporarily failed (${account_name}): ${reason} @ ${timestamp}`);
      }
    });

    return () => {
      unlisten.then(f => f());
    };
  }, []);

  // Listen for proxy switch/ban events
  const [proxyNotice, setProxyNotice] = useState<string | null>(null);
  useEffect(() => {
    const unsub1 = listen<string>('proxy-account-switched', (e) => {
      const msg = `Proxy auto-switched → ${e.payload}`;
      setProxyNotice(msg);
      setTimeout(() => setProxyNotice(null), 8000);
      refresh();
      checkProxyStatus();
    });
    const unsub2 = listen<string>('proxy-account-banned', (e) => {
      const msg = `Ban detected: ${e.payload}, auto-switched`;
      setProxyNotice(msg);
      setTimeout(() => setProxyNotice(null), 10000);
      refresh();
    });
    const unsub3 = listen<string>('proxy-all-exhausted', (e) => {
      setProxyNotice(e.payload);
      setTimeout(() => setProxyNotice(null), 15000);
    });
    return () => {
      unsub1.then(f => f());
      unsub2.then(f => f());
      unsub3.then(f => f());
    };
  }, [refresh]);

  // Listen for settings update events
  useEffect(() => {
    const unlisten = listen('settings-updated', () => {
      console.log('[Frontend] Received settings update notification, reloading settings');
      refresh();
      checkProxyStatus();
    });

    return () => {
      unlisten.then(f => f());
    };
  }, [refresh]);

  // Execute actual switch logic
  const performSwitch = async (id: string) => {
    await switchTo(id);
    if (settings.auto_reload_ide) {
      setTimeout(async () => {
        await reloadIdeWindows(false);
      }, 300);
    }
    setTimeout(() => {
      refreshUsage();
    }, 500);
  };

  // Switch account (with conflict detection)
  const handleSwitch = async (id: string) => {
    if (isSwitching) return;
    try {
      setIsSwitching(true);
      // 1. Check for unsynced official Token updates
      const conflictName = await checkSyncConflict();

      if (conflictName) {
        // 2. If conflict exists, store target ID and show confirmation dialog
        setConflictAccountName(conflictName);
        setPendingSwitchId(id);
        setShowConflictModal(true);
        return;
      }

      // 3. No conflict, switch directly
      await performSwitch(id);
    } catch (err) {
      console.error('Switch check failed:', err);
      // Attempt conservative switch
      try {
        await performSwitch(id);
      } catch (switchErr) {
        // switchTo has already setError internally, but we can log here too
        console.error('Conservative switch also failed:', switchErr);
      }
    } finally {
      setIsSwitching(false);
      checkSyncStatus();
    }
  };

  // Confirm overwrite
  const handleConfirmSwitch = async () => {
    if (!pendingSwitchId || isSwitching) return;
    try {
      setIsSwitching(true);
      await performSwitch(pendingSwitchId);
      setShowConflictModal(false);
      setPendingSwitchId(null);
    } catch (err) {
      console.error('Confirm switch failed:', err);
      // switchTo has already setError internally, just close the modal here so user sees Banner error
      setShowConflictModal(false);
    } finally {
      setIsSwitching(false);
      checkSyncStatus();
    }
  };

  // Sync with IDE state
  const handleFollowIdeAction = async () => {
    try {
      setIsSwitching(true);
      await syncActiveWithDisk();
      setShowConflictModal(false);
      setPendingSwitchId(null);
      await checkSyncStatus();
    } catch (err) {
      console.error('Sync IDE state failed:', err);
    } finally {
      setIsSwitching(false);
    }
  };

  // Cancel switch
  const handleCancelSwitch = () => {
    setShowConflictModal(false);
    setPendingSwitchId(null);
  };

  const handleExport = async () => {
    try {
      const json = await exportAccounts();
      const path = await save({
        filters: [{
          name: 'JSON',
          extensions: ['json']
        }],
        defaultPath: `codex-accounts-${new Date().toISOString().slice(0, 10)}.json`
      });

      if (path) {
        await writeTextFile(path, json);
        alert('Export successful!');
      }
    } catch (err) {
      alert('Export failed: ' + String(err));
    }
  };


  if (loading) {
    return (
      <div className="app" data-palette={settings.theme_palette || 'github'}>
        <div className="loading">
          <div className="spinner" />
          <p>Loading...</p>
        </div>
      </div>
    );
  }

  return (
    <div className="app" data-palette={settings.theme_palette || 'github'}>
      {/* Top header */}
      <header className="app-header">
        <div className="header-left">
          <div className="app-logo">
            <Zap size={18} />
          </div>
          <h1>Codex Switcher <span className="app-version">v0.3.0</span></h1>
          <div className={`proxy-indicator ${proxyRunning ? 'on' : 'off'}`} title={proxyRunning ? 'Proxy running' : 'Proxy stopped'}>
            <span className="proxy-dot" />
            {proxyRunning ? 'Proxy ON' : 'Proxy OFF'}
          </div>
        </div>

        {/* Navigation */}
        <nav className="header-nav">
          <button
            className={`nav-item ${currentPage === 'dashboard' ? 'active' : ''}`}
            onClick={() => setCurrentPage('dashboard')}
          >
            Dashboard
          </button>
          <button
            className={`nav-item ${currentPage === 'accounts' ? 'active' : ''}`}
            onClick={() => setCurrentPage('accounts')}
          >
            Accounts
          </button>
          <button
            className={`nav-item ${currentPage === 'proxy' ? 'active' : ''}`}
            onClick={() => setCurrentPage('proxy')}
          >
            Proxy
          </button>
          <button
            className={`nav-item ${currentPage === 'stats' ? 'active' : ''}`}
            onClick={() => setCurrentPage('stats')}
          >
            Usage
          </button>
          <button
            className={`nav-item ${currentPage === 'skills' ? 'active' : ''}`}
            onClick={() => setCurrentPage('skills')}
          >
            Skills
          </button>
          <button
            className={`nav-item ${currentPage === 'settings' ? 'active' : ''}`}
            onClick={() => setCurrentPage('settings')}
          >
            Settings
          </button>
        </nav>

        <div className="header-actions">
          <button className="btn btn-primary" onClick={() => setShowAddModal(true)}>
            + Add Account
          </button>
        </div>
      </header>

      {(error || schedulerError) && (
        <div className="error-banner">
          {error && <div>{error}</div>}
          {schedulerError && <div>{schedulerError}</div>}
        </div>
      )}

      {proxyNotice && (
        <div className="proxy-notice-banner" onClick={() => setProxyNotice(null)}>
          {proxyNotice}
        </div>
      )}

      <main className="app-main">
        {currentPage === 'dashboard' ? (
          <Dashboard
            accounts={accounts}
            currentAccount={currentAccount}
            usage={usage}
            usageLoading={usageLoading}
            usageError={usageError}
            isCurrentInvalid={currentAccount?.cached_quota?.is_valid_for_cli === false}
            onSwitch={handleSwitch}
            onRefreshUsage={refreshUsage}
            onNavigateToAccounts={() => setCurrentPage('accounts')}
            onExport={handleExport}
            syncStatus={syncStatus}
            onSyncWithDisk={async () => {
              try {
                await syncActiveWithDisk();
                checkSyncStatus();
              } catch (err) {
                console.error('Synch failed:', err);
              }
            }}
            onImportDiskAccount={async (name) => {
              try {
                await importCurrent(name, 'Auto-imported from IDE');
                checkSyncStatus();
              } catch (err) {
                console.error('Import failed:', err);
              }
            }}
          />
        ) : currentPage === 'accounts' ? (
          <AccountList
            accounts={accounts}
            currentId={currentId}
            settings={settings}
            onSwitch={handleSwitch}
            onDelete={deleteAccount}
            onSetInactiveRefreshEnabled={setInactiveRefreshEnabled}
            onUpdateSettings={updateSettings}
            onRefreshComplete={refresh}
            onAddAccount={() => setShowAddModal(true)}
            onRefreshUsage={refreshUsage}
            usageLoading={usageLoading}
          />
        ) : currentPage === 'proxy' ? (
          <Proxy />
        ) : currentPage === 'stats' ? (
          <Stats />
        ) : currentPage === 'skills' ? (
          <Skills />
        ) : (
          <Settings />
        )}
      </main>

      <AddAccountModal
        isOpen={showAddModal}
        onClose={() => setShowAddModal(false)}
        onAdd={importCurrent}
        onSuccess={refresh}
      />

      <ConfirmModal
        isOpen={showConflictModal}
        title="⚠️ Session Conflict Warning"
        message={
          <>
            <p>Unsynced token update detected in official Codex plugin.</p>
            <p>Current account state conflicts with official file:</p>
            <span className="confirm-account-name">{conflictAccountName || 'Active Account'}</span>
            <p style={{ marginTop: '12px' }}>
              Switching will <b>overwrite</b> the current login state in the official plugin; unsynced updates will be lost.
            </p>
          </>
        }
        confirmText="Overwrite & Switch"
        cancelText="Cancel"
        onConfirm={handleConfirmSwitch}
        onCancel={handleCancelSwitch}
        isLoading={isSwitching}
        extraActionText="Sync with IDE"
        onExtraAction={handleFollowIdeAction}
      />
    </div>
  );
}

export default App;
