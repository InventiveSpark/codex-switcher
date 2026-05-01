import { useState, useEffect } from 'react';

/**
 * Hook to calculate remaining time until a reset timestamp
 * @param resetAt Unix timestamp in seconds
 */
export function useCountdown(resetAt?: number) {
    const [timeLeft, setTimeLeft] = useState<string>('');

    useEffect(() => {
        if (!resetAt || resetAt <= 0) {
            setTimeLeft('');
            return;
        }

        const update = () => {
            const now = Math.floor(Date.now() / 1000);
            const diff = resetAt - now;

            if (diff <= 0) {
                setTimeLeft('Resetting soon');
                return;
            }

            const days = Math.floor(diff / 86400);
            const hours = Math.floor((diff % 86400) / 3600);
            const minutes = Math.floor((diff % 3600) / 60);
            const seconds = diff % 60;

            if (days > 0) {
                setTimeLeft(`Resets in ${days}d ${hours}h ${minutes}m`);
            } else if (hours > 0) {
                setTimeLeft(`Resets in ${hours}h ${minutes}m ${seconds}s`);
            } else if (minutes > 0) {
                setTimeLeft(`Resets in ${minutes}m ${seconds}s`);
            } else {
                setTimeLeft(`Resets in ${seconds}s`);
            }
        };

        update();
        const timer = setInterval(update, 1000);
        return () => clearInterval(timer);
    }, [resetAt]);

    return timeLeft;
}

/**
 * Minimal version of the countdown for table/list views (e.g. "4h 59m")
 */
export function useShortCountdown(resetAt?: number) {
    const [timeLeft, setTimeLeft] = useState<string>('');

    useEffect(() => {
        if (!resetAt || resetAt <= 0) {
            setTimeLeft('');
            return;
        }

        const update = () => {
            const now = Math.floor(Date.now() / 1000);
            const diff = resetAt - now;

            if (diff <= 0) {
                setTimeLeft('--');
                return;
            }

            const days = Math.floor(diff / 86400);
            const hours = Math.floor((diff % 86400) / 3600);
            const minutes = Math.floor((diff % 3600) / 60);

            if (days > 0) {
                setTimeLeft(`${days}d ${hours}h ${minutes}m`);
            } else if (hours > 0) {
                setTimeLeft(`${hours}h ${minutes}m`);
            } else if (minutes > 0) {
                setTimeLeft(`${minutes}m`);
            } else {
                setTimeLeft(`${diff}s`);
            }
        };

        update();
        const timer = setInterval(update, 1000);
        return () => clearInterval(timer);
    }, [resetAt]);

    return timeLeft;
}
