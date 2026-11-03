type NavigationProgressControllerOptions = {
	delayMs: number;
	minVisibleMs: number;
	onVisibleChange: (visible: boolean) => void;
	now?: () => number;
};

export type NavigationProgressController = {
	start: () => void;
	finish: () => void;
	cleanup: () => void;
};

export function createNavigationProgressController({
	delayMs,
	minVisibleMs,
	onVisibleChange,
	now = () => Date.now()
}: NavigationProgressControllerOptions): NavigationProgressController {
	let delayTimer: ReturnType<typeof setTimeout> | null = null;
	let hideTimer: ReturnType<typeof setTimeout> | null = null;
	let shownAt = 0;
	let isVisible = false;

	function clearTimers() {
		if (delayTimer) {
			clearTimeout(delayTimer);
			delayTimer = null;
		}
		if (hideTimer) {
			clearTimeout(hideTimer);
			hideTimer = null;
		}
	}

	function setVisible(nextVisible: boolean) {
		if (isVisible === nextVisible) return;
		isVisible = nextVisible;
		onVisibleChange(nextVisible);
	}

	return {
		start() {
			if (hideTimer) {
				clearTimeout(hideTimer);
				hideTimer = null;
			}
			if (delayTimer) {
				clearTimeout(delayTimer);
			}

			delayTimer = setTimeout(() => {
				shownAt = now();
				setVisible(true);
				delayTimer = null;
			}, delayMs);
		},
		finish() {
			if (delayTimer) {
				clearTimeout(delayTimer);
				delayTimer = null;
				return;
			}

			if (!isVisible) {
				return;
			}

			const remaining = minVisibleMs - (now() - shownAt);
			if (remaining <= 0) {
				setVisible(false);
				return;
			}

			if (hideTimer) {
				clearTimeout(hideTimer);
			}
			hideTimer = setTimeout(() => {
				setVisible(false);
				hideTimer = null;
			}, remaining);
		},
		cleanup() {
			clearTimers();
			setVisible(false);
		}
	};
}
