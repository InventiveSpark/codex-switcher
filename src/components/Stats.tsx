import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    AreaChart, Area, BarChart, Bar, PieChart, Pie, Cell,
    XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, Legend
} from 'recharts';
import './Stats.css';

interface TokenHistoryEntry {
    timestamp: string;
    model: string;
    input_tokens: number;
    output_tokens: number;
    cost: number;
}

interface SwitchEvent {
    timestamp: string;
    from_account: string | null;
    to_account: string;
    reason: string;
    from_quota_5h: number | null;
    to_quota_5h: number | null;
}

interface SwitchStats {
    today_count: number;
    week_count: number;
    total_count: number;
    by_reason: Record<string, number>;
    by_account: Record<string, number>;
}

interface TokenStats {
    total_input_tokens: number;
    total_output_tokens: number;
    total_tokens: number;
    total_cost_usd: number;
    total_requests: number;
}

const COLORS = ['#8b5cf6', '#10b981', '#f59e0b', '#ef4444', '#3b82f6', '#ec4899', '#14b8a6', '#f97316'];

function formatTokens(n: number): string {
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K';
    return n.toString();
}

function formatTime(ts: string): string {
    const d = new Date(ts);
    return d.toLocaleString('en-US', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}

function formatDuration(from: string, to: string): string {
    const diff = new Date(to).getTime() - new Date(from).getTime();
    if (diff < 0) return '-';
    const mins = Math.floor(diff / 60000);
    const hours = Math.floor(mins / 60);
    if (hours > 0) return `${hours}h ${mins % 60}m`;
    return `${mins}m`;
}

type TimeRange = 'day' | 'week' | 'month';

export function Stats() {
    const [range, setRange] = useState<TimeRange>('week');
    const [tokenHistory, setTokenHistory] = useState<TokenHistoryEntry[]>([]);
    const [switchHistory, setSwitchHistory] = useState<SwitchEvent[]>([]);
    const [switchStats, setSwitchStats] = useState<SwitchStats | null>(null);
    const [tokenStats, setTokenStats] = useState<TokenStats | null>(null);

    const days = range === 'day' ? 1 : range === 'week' ? 7 : 30;

    const fetchData = async () => {
        try {
            const [th, sh, ss, ts] = await Promise.all([
                invoke<TokenHistoryEntry[]>('get_token_history', { days }),
                invoke<SwitchEvent[]>('get_switch_history', { days }),
                invoke<SwitchStats>('get_switch_stats'),
                invoke<TokenStats>('get_token_stats'),
            ]);
            setTokenHistory(th);
            setSwitchHistory(sh.reverse());
            setSwitchStats(ss);
            setTokenStats(ts);
        } catch (e) {
            console.error('Failed to load stats:', e);
        }
    };

    useEffect(() => { fetchData(); }, [range]);

    // Aggregate token trend data (by hour/day)
    const trendData = (() => {
        const buckets: Record<string, { label: string; input: number; output: number; cost: number }> = {};
        for (const entry of tokenHistory) {
            const d = new Date(entry.timestamp);
            const key = range === 'day'
                ? d.toLocaleTimeString('en-US', { hour: '2-digit' })
                : d.toLocaleDateString('en-US', { month: 'numeric', day: 'numeric' });
            if (!buckets[key]) buckets[key] = { label: key, input: 0, output: 0, cost: 0 };
            buckets[key].input += entry.input_tokens;
            buckets[key].output += entry.output_tokens;
            buckets[key].cost += entry.cost;
        }
        return Object.values(buckets);
    })();

    // Model distribution (pie chart)
    const modelData = (() => {
        const map: Record<string, number> = {};
        for (const entry of tokenHistory) {
            map[entry.model] = (map[entry.model] || 0) + entry.input_tokens + entry.output_tokens;
        }
        return Object.entries(map).map(([name, value]) => ({ name, value }));
    })();

    // Switch reason distribution
    const reasonData = switchStats
        ? Object.entries(switchStats.by_reason).map(([name, value]) => ({ name, value }))
        : [];

    const accountCount = switchStats ? Object.keys(switchStats.by_account).length : 0;

    // Separate regular switches from system background tasks
    const actualSwitches = switchHistory.filter(e => e.reason !== 'Auto Refresh' && e.reason !== 'Background Keepalive');
    const systemLogs = switchHistory.filter(e => e.reason === 'Auto Refresh' || e.reason === 'Background Keepalive');

    return (
        <div className="stats-page">
            <div className="stats-header">
                <h2>Stats</h2>
                <div className="time-range-btns">
                    {(['day', 'week', 'month'] as TimeRange[]).map(r => (
                        <button
                            key={r}
                            className={`range-btn ${range === r ? 'active' : ''}`}
                            onClick={() => setRange(r)}
                        >
                            {r === 'day' ? 'Day' : r === 'week' ? 'Week' : 'Month'}
                        </button>
                    ))}
                </div>
            </div>

            {/* Summary Cards */}
            <div className="stats-cards">
                <div className="stat-card purple">
                    <div className="stat-card-value">{formatTokens(tokenStats?.total_tokens ?? 0)}</div>
                    <div className="stat-card-label">Total Tokens</div>
                </div>
                <div className="stat-card yellow">
                    <div className="stat-card-value">${(tokenStats?.total_cost_usd ?? 0).toFixed(2)}</div>
                    <div className="stat-card-label">Total Cost</div>
                </div>
                <div className="stat-card green">
                    <div className="stat-card-value">{switchStats?.total_count ?? 0}</div>
                    <div className="stat-card-label">Switch Count</div>
                </div>
                <div className="stat-card blue">
                    <div className="stat-card-value">{accountCount}</div>
                    <div className="stat-card-label">Accounts Used</div>
                </div>
            </div>

            {/* Token Trend Chart */}
            {trendData.length > 0 && (
                <div className="stats-section">
                    <h3>Token Trend</h3>
                    <ResponsiveContainer width="100%" height={250}>
                        <AreaChart data={trendData}>
                            <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.06)" />
                            <XAxis dataKey="label" stroke="rgba(255,255,255,0.3)" fontSize={11} />
                            <YAxis stroke="rgba(255,255,255,0.3)" fontSize={11} tickFormatter={formatTokens} />
                            <Tooltip
                                contentStyle={{ background: '#1e1245', border: '1px solid rgba(255,255,255,0.1)', borderRadius: 8 }}
                                labelStyle={{ color: '#fff' }}
                            />
                            <Area type="monotone" dataKey="input" stackId="1" stroke="#8b5cf6" fill="#8b5cf6" fillOpacity={0.4} name="Input" />
                            <Area type="monotone" dataKey="output" stackId="1" stroke="#10b981" fill="#10b981" fillOpacity={0.4} name="Output" />
                            <Legend />
                        </AreaChart>
                    </ResponsiveContainer>
                </div>
            )}

            {/* Bottom: Cost + Model Distribution */}
            <div className="stats-grid">
                {trendData.length > 0 && (
                    <div className="stats-section">
                        <h3>Cost Trend</h3>
                        <ResponsiveContainer width="100%" height={200}>
                            <BarChart data={trendData}>
                                <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.06)" />
                                <XAxis dataKey="label" stroke="rgba(255,255,255,0.3)" fontSize={11} />
                                <YAxis stroke="rgba(255,255,255,0.3)" fontSize={11} tickFormatter={v => `$${v}`} />
                                <Tooltip
                                    contentStyle={{ background: '#1e1245', border: '1px solid rgba(255,255,255,0.1)', borderRadius: 8 }}
                                    formatter={(v) => [`$${Number(v).toFixed(4)}`, 'Cost']}
                                />
                                <Bar dataKey="cost" fill="#fbbf24" radius={[4, 4, 0, 0]} />
                            </BarChart>
                        </ResponsiveContainer>
                    </div>
                )}

                {(modelData.length > 0 || reasonData.length > 0) && (
                    <div className="stats-section">
                        <h3>{modelData.length > 0 ? 'Model Distribution' : 'Switch Reasons'}</h3>
                        <ResponsiveContainer width="100%" height={200}>
                            <PieChart>
                                <Pie
                                    data={modelData.length > 0 ? modelData : reasonData}
                                    cx="50%"
                                    cy="50%"
                                    innerRadius={50}
                                    outerRadius={80}
                                    paddingAngle={3}
                                    dataKey="value"
                                >
                                    {(modelData.length > 0 ? modelData : reasonData).map((_, i) => (
                                        <Cell key={i} fill={COLORS[i % COLORS.length]} />
                                    ))}
                                </Pie>
                                <Tooltip
                                    contentStyle={{ background: '#1e1245', border: '1px solid rgba(255,255,255,0.1)', borderRadius: 8 }}
                                />
                                <Legend />
                            </PieChart>
                        </ResponsiveContainer>
                    </div>
                )}
            </div>

            {/* Switch Log */}
            <div className="stats-section">
                <h3>Switch Log ({actualSwitches.length})</h3>
                <div className="switch-log-table">
                    <div className="log-header">
                        <span>Time</span>
                        <span>Switch Path</span>
                        <span>Reason</span>
                        <span>Duration</span>
                    </div>
                    {actualSwitches.length === 0 ? (
                        <div className="log-empty">No switch records</div>
                    ) : (
                        actualSwitches.map((e, i) => (
                            <div key={i} className="log-row">
                                <span className="log-time">{formatTime(e.timestamp)}</span>
                                <span className="log-path">
                                    {e.from_account ? (
                                        <>{shortName(e.from_account)} → {shortName(e.to_account)}</>
                                    ) : (
                                        <>→ {shortName(e.to_account)}</>
                                    )}
                                </span>
                                <span className={`log-reason ${reasonClass(e.reason)}`}>{e.reason}</span>
                                <span className="log-duration">
                                    {i < actualSwitches.length - 1
                                        ? formatDuration(actualSwitches[i + 1].timestamp, e.timestamp)
                                        : '-'}
                                </span>
                            </div>
                        ))
                    )}
                </div>
            </div>

            {/* Background Task Log */}
            {systemLogs.length > 0 && (
                <div className="stats-section">
                    <h3>Background Task Log ({systemLogs.length})</h3>
                    <div className="switch-log-table">
                        <div className="log-header">
                            <span>Time</span>
                            <span>Target Account</span>
                            <span>Task Type</span>
                            <span>Quota After Refresh</span>
                        </div>
                        {systemLogs.map((e, i) => (
                            <div key={`sys-${i}`} className="log-row">
                                <span className="log-time">{formatTime(e.timestamp)}</span>
                                <span className="log-path">
                                    {shortName(e.to_account)}
                                </span>
                                <span className={`log-reason ${reasonClass(e.reason)}`}>{e.reason}</span>
                                <span className="log-duration" style={{ color: 'var(--success-color, #10b981)' }}>
                                    {e.to_quota_5h !== null ? `${e.to_quota_5h}%` : 'Success'}
                                </span>
                            </div>
                        ))}
                    </div>
                </div>
            )}
        </div>
    );
}

function shortName(name: string): string {
    if (name.length > 18) return name.slice(0, 15) + '...';
    return name;
}

function reasonClass(reason: string): string {
    if (reason.includes('Manual')) return 'manual';
    if (reason.includes('429') || reason.includes('Quota')) return 'ratelimit';
    if (reason.includes('Banned')) return 'banned';
    if (reason.includes('Keepalive')) return 'keepalive';
    if (reason.includes('Refresh')) return 'refresh';
    return 'auto';
}
