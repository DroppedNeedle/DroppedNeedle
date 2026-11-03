import { writable } from 'svelte/store';

interface Toast {
	message: string;
	type: 'success' | 'error' | 'info' | 'warning';
	duration?: number;
	/** One button beside the message, such as Undo. The toast closes when it runs. */
	action?: { label: string; run: () => unknown };
}

function createToastStore() {
	const { subscribe, set } = writable<Toast | null>(null);
	let timer: ReturnType<typeof setTimeout> | null = null;

	function clearTimer(): void {
		if (timer) clearTimeout(timer);
		timer = null;
	}

	function show(toast: Toast): void {
		clearTimer();
		set(toast);
		timer = setTimeout(() => {
			timer = null;
			set(null);
		}, toast.duration ?? 3000);
	}

	return {
		subscribe,
		show,
		notify: (message: string, type: Toast['type'] = 'info') => show({ message, type }),
		hide: () => {
			clearTimer();
			set(null);
		}
	};
}

export const toastStore = createToastStore();
