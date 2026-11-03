const STORAGE_KEY = 'droppedneedle-dismissed-prompts';

const isBrowser = typeof localStorage !== 'undefined';

function getDismissed(): Set<string> {
	if (!isBrowser) return new Set();
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		return raw ? new Set(JSON.parse(raw)) : new Set();
	} catch {
		return new Set();
	}
}

function saveDismissed(dismissed: Set<string>): void {
	if (!isBrowser) return;
	localStorage.setItem(STORAGE_KEY, JSON.stringify([...dismissed]));
}

export function isDismissed(service: string): boolean {
	return getDismissed().has(service);
}

export function dismiss(service: string): void {
	const dismissed = getDismissed();
	dismissed.add(service);
	saveDismissed(dismissed);
}
