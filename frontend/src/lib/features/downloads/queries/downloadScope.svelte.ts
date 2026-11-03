import { SvelteSet } from 'svelte/reactivity';

let scope = $state({ userId: null as string | null, role: 'anon', generation: 0 });
const listeners = new SvelteSet<(userId: string) => void>();

export function getDownloadScope() {
	return scope;
}

export function setDownloadScope(userId: string | null, role = 'anon') {
	if (scope.userId === userId && scope.role === role) return;
	const previous = scope;
	scope = { userId, role, generation: previous.generation + 1 };
	if (previous.userId === userId && userId !== null) {
		for (const listener of listeners) listener(userId);
	}
}

export function subscribeDownloadRoleChange(listener: (userId: string) => void) {
	listeners.add(listener);
	return () => listeners.delete(listener);
}
