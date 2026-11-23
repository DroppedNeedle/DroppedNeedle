import { writable } from 'svelte/store';

/** One button on a toast. The toast closes when it runs. */
export interface ToastAction {
	label: string;
	run: () => unknown;
}

interface Toast {
	message: string;
	/** Bold words before the message. */
	title?: string;
	type: 'success' | 'error' | 'info' | 'warning';
	duration?: number;
	/** One button beside the message, such as Undo. */
	action?: ToastAction;
	/**
	 * A further question after the action and up to three answers to it, as
	 * in "Undo, or was it: Know it already, Not now".
	 */
	choices?: { ask: string; actions: ToastAction[] };
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
