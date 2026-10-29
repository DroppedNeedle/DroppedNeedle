export function formatListenCount(count: number | null, compact = false): string {
	if (count == null) return '';
	const suffix = compact ? '' : ' plays';
	if (count >= 1_000_000_000) return `${(count / 1_000_000_000).toFixed(1)}B${suffix}`;
	if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M${suffix}`;
	if (count >= 1_000) return `${(count / 1_000).toFixed(1)}K${suffix}`;
	return `${count}${suffix}`;
}

export function formatListenedAt(timestamp: string | null): string {
	if (!timestamp) return '';
	const date = new Date(timestamp);
	const now = new Date();
	const diffMs = now.getTime() - date.getTime();
	const diffMins = Math.floor(diffMs / 60000);
	const diffHours = Math.floor(diffMins / 60);
	const diffDays = Math.floor(diffHours / 24);

	if (diffMins < 1) return 'Just now';
	if (diffMins < 60) return `${diffMins}m ago`;
	if (diffHours < 24) return `${diffHours}h ago`;
	if (diffDays < 7) return `${diffDays}d ago`;
	return date.toLocaleDateString();
}

export function formatDuration(ms?: number | null): string {
	if (!ms) return '--:--';
	const totalSeconds = Math.floor(ms / 1000);
	const minutes = Math.floor(totalSeconds / 60);
	const seconds = totalSeconds % 60;
	return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

export function formatDurationSec(sec?: number | null): string {
	if (!sec && sec !== 0) return '--:--';
	const total = Math.floor(sec);
	const minutes = Math.floor(total / 60);
	const seconds = total % 60;
	return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

export function formatTotalDuration(ms?: number | null): string {
	if (!ms) return '';
	const totalSeconds = Math.floor(ms / 1000);
	const hours = Math.floor(totalSeconds / 3600);
	const minutes = Math.floor((totalSeconds % 3600) / 60);

	if (hours > 0) {
		return `${hours} hr ${minutes} min`;
	}
	return `${minutes} min`;
}

export function formatTotalDurationSec(sec?: number | null): string {
	if (!sec) return '';
	const hours = Math.floor(sec / 3600);
	const minutes = Math.floor((sec % 3600) / 60);

	if (hours > 0) {
		return `${hours} hr ${minutes} min`;
	}
	return `${minutes} min`;
}

export function formatRelativeTime(date: Date): string {
	const now = new Date();
	const diffMs = now.getTime() - date.getTime();
	const diffMins = Math.floor(diffMs / 60000);
	const diffHours = Math.floor(diffMins / 60);
	const diffDays = Math.floor(diffHours / 24);

	if (diffMins < 1) return 'Just now';
	if (diffMins < 60) return `${diffMins}m ago`;
	if (diffHours < 24) return `${diffHours}h ago`;
	if (diffDays < 7) return `${diffDays}d ago`;
	return date.toLocaleDateString();
}

export function isValidMbid(id: string | null | undefined): boolean {
	if (!id) return false;
	const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
	return UUID_PATTERN.test(id);
}

export function formatLastUpdated(date: Date | null): string {
	if (!date) return '';
	const now = new Date();
	const diffMs = now.getTime() - date.getTime();
	const diffMins = Math.floor(diffMs / 60000);

	if (diffMins < 1) return 'Just now';
	if (diffMins < 60) return `${diffMins}m ago`;
	const diffHours = Math.floor(diffMins / 60);
	if (diffHours < 24) return `${diffHours}h ago`;
	return date.toLocaleDateString();
}

export function formatBytes(bytes: number | null | undefined): string {
	if (!bytes || bytes <= 0) return '0 B';
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
	const value = bytes / Math.pow(1024, i);
	return `${value.toFixed(value >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}

export function countryToFlag(code: string | null): string {
	if (!code || code.length !== 2) return '';
	return String.fromCodePoint(
		...code
			.toUpperCase()
			.split('')
			.map((c) => 0x1f1e6 + c.charCodeAt(0) - 65)
	);
}

/** A greeting for the time of day. */
export function getGreeting(): string {
	const hour = new Date().getHours();
	if (hour < 12) return 'Good morning';
	if (hour < 18) return 'Good afternoon';
	return 'Good evening';
}
