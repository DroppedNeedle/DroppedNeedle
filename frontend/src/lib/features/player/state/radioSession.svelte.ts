function createRadioSession() {
	let active = $state(false);
	let generation = 0;
	let controller: AbortController | null = null;

	return {
		get active() {
			return active;
		},

		beginLaunch() {
			controller?.abort();
			controller = new AbortController();
			active = false;
			generation += 1;
			return { generation, signal: controller.signal };
		},

		isCurrent(candidate: number) {
			return candidate === generation;
		},

		start(candidate: number) {
			if (candidate !== generation) return false;
			controller = null;
			active = true;
			return true;
		},

		end() {
			controller?.abort();
			controller = null;
			generation += 1;
			active = false;
		}
	};
}

export const radioSession = createRadioSession();
