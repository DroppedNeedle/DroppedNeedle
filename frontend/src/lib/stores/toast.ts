import { writable } from 'svelte/store';

interface Toast {
	message: string;
	type: 'success' | 'error' | 'info' | 'warning';
	duration?: number;
}

function createToastStore() {
	const { subscribe, set } = writable<Toast | null>(null);
	return {
		subscribe,
		show: (toast: Toast) => {
			set(toast);
			setTimeout(() => set(null), toast.duration ?? 3000);
		},
		hide: () => set(null)
	};
}

export const toastStore = createToastStore();
