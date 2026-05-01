import { UsageDisplay } from '../hooks/useUsage';
import { Account } from '../hooks/useAccounts';
import { StatsBar } from './StatsBar';
import { UsageCard } from './UsageCard';
import './Dashboard.css';

interface DashboardProps {
    accounts: Account[];
    currentAccount: Account | null;
    usage: UsageDisplay | null;
    usageLoading: boolean;
    usageError: string | null;
    isCurrentInvalid?: boolean;
    onSwitch: (id: string) => void;
    onRefreshUsage: () => void;
    onNavigateToAccounts: () => void;
    onExport: () => void;
    syncStatus?: {
        is_synced: boolean;
        disk_email: string | null;
        matching_id: string | null;
    };
    onSyncWithDisk: () => void;
    onImportDiskAccount: (name: string) => void;
}

export function Dashboard({
    accounts,
    currentAccount,
    usage,
    usageLoading,
    usageError,
    isCurrentInvalid,
    onSwitch,
    onRefreshUsage,
    onNavigateToAccounts,
    onExport,
    syncStatus,
    onSyncWithDisk,
    onImportDiskAccount,
}: DashboardProps) {
    // Get best account recommendation (highest quota)
    const getBestAccount = () => {
        if (accounts.length === 0) return null;
        // Simply return the first non-current account
        return accounts.find(a => a.id !== currentAccount?.id) || null;
    };

    const bestAccount = getBestAccount();

    return (
        <div className="dashboard">
            {/* Greeting */}
            <div className="dashboard-greeting">
                <h2>
                    Hello, {currentAccount?.name.split('@')[0] || 'User'} 👋
                </h2>
            </div>

            {/* Stats Cards */}
            <StatsBar accountCount={accounts.length} usage={usage} />

            {/* Sync Status Warning */}
            {syncStatus && !syncStatus.is_synced && (
                <div className="sync-warning-banner">
                    <div className="banner-content">
                        <span className="banner-icon">⚠️</span>
                        <div className="banner-text">
                            <strong>Session Mismatch:</strong>
                            Detected IDE using <span>{syncStatus.disk_email || 'Unknown Account'}</span>
                        </div>
                    </div>
                    <div className="banner-actions">
                        {syncStatus.matching_id ? (
                            <button className="btn btn-sm btn-accent" onClick={onSyncWithDisk}>
                                Fix Active State
                            </button>
                        ) : (
                            <button className="btn btn-sm btn-primary" onClick={() => onImportDiskAccount(syncStatus.disk_email || 'New Account')}>
                                Import This Account
                            </button>
                        )}
                    </div>
                </div>
            )}

            {/* Two-column Layout */}
            <div className="dashboard-grid">
                {/* Current Account */}
                <div className={`dashboard-card current-account ${isCurrentInvalid ? 'invalid' : ''}`}>
                    <div className="card-header">
                        <span className="card-icon">✓</span>
                        <h3>Current Account</h3>
                        {isCurrentInvalid && <span className="invalid-badge" title="Auth expired, please delete and re-login">⚠️ Invalid</span>}
                    </div>
                    {currentAccount ? (
                        <div className="current-account-content">
                            <div className="account-info">
                                <span className="email-icon">✉</span>
                                <span className="email">{currentAccount.name}</span>
                                {usage?.plan_type && (
                                    <span className="plan-badge">{usage.plan_type.toUpperCase()}</span>
                                )}
                            </div>

                            <UsageCard
                                usage={usage}
                                loading={usageLoading}
                                error={usageError}
                                onRefresh={onRefreshUsage}
                            />

                            <button
                                className="btn btn-outline btn-full"
                                onClick={onNavigateToAccounts}
                            >
                                Switch Account
                            </button>
                        </div>
                    ) : (
                        <div className="no-account">
                            <p>No Account</p>
                        </div>
                    )}
                </div>

                {/* Best Account Recommendation */}
                <div className="dashboard-card best-accounts">
                    <div className="card-header">
                        <span className="card-icon">↗</span>
                        <h3>Best Account Recommendation</h3>
                    </div>
                    <div className="best-accounts-list">
                        {bestAccount ? (
                            <div className="best-account-item">
                                <div className="account-label">
                                    <span className="label-text">Recommended Account</span>
                                    <span className="account-email">{bestAccount.name}</span>
                                </div>
                                <span className="quota-badge">100%</span>
                            </div>
                        ) : (
                            <p className="no-recommendation">No Recommendations</p>
                        )}
                    </div>
                    {accounts.length > 1 && (
                        <button
                            className="btn btn-accent btn-full"
                            onClick={() => bestAccount && onSwitch(bestAccount.id)}
                        >
                            Switch to Best
                        </button>
                    )}
                </div>
            </div>

            {/* Quick Links */}
            <div className="dashboard-links">
                <button className="link-card" onClick={onNavigateToAccounts}>
                    <span>View All Accounts</span>
                    <span className="link-arrow">→</span>
                </button>
                <button className="link-card" onClick={onExport}>
                    <span>Export Account Data</span>
                    <span className="link-icon">↓</span>
                </button>
            </div>
        </div>
    );
}
