import { writable } from 'svelte/store';
import { splitErrorRef } from '$lib/api/errors';

const TOAST_MS = 3000;
/** Long enough to read and copy an error's ref. */
const REF_TOAST_MS = 10000;

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
	/** The ref an error message ends with; the store takes it out of the message. */
	ref?: string;
}

function createToastStore() {
	const { subscribe, set } = writable<Toast | null>(null);
	let timer: ReturnType<typeof setTimeout> | null = null;

	function clearTimer(): void {
		if (timer) clearTimeout(timer);
		timer = null;
	}

	function show(toast: Toast): void {
		const { text, ref } =
			toast.type === 'error' ? splitErrorRef(toast.message) : { text: toast.message, ref: null };
		const duration = ref
			? Math.max(toast.duration ?? 0, REF_TOAST_MS)
			: (toast.duration ?? TOAST_MS);
		clearTimer();
		set({ ...toast, message: text, ref: ref ?? undefined });
		timer = setTimeout(() => {
			timer = null;
			set(null);
		}, duration);
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
