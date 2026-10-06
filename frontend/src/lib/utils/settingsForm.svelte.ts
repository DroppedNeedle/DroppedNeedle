import { api, ApiError } from '$lib/api/client';
import { isAbortError } from '$lib/utils/errorHandling';
import { integrationStore } from '$lib/stores/integration';
import type { V3Url } from '$lib/api/v3/endpoint';
import { HOME_ENDPOINTS } from '$lib/queries/charts/endpoints';

export interface SettingsFormConfig<T> {
	loadEndpoint: V3Url;
	saveEndpoint: V3Url;
	testEndpoint?: V3Url;
	defaultValue?: T;
	enabledField?: keyof T;
	/** A second toggle (e.g. "allow login") that should also stay locked until a test succeeds. */
	secondaryEnabledField?: keyof T;
	refreshIntegration?: boolean;
	afterSave?: (data: T) => void | Promise<void>;
	afterTest?: (result: unknown) => void;
}

export function createSettingsForm<T>(config: SettingsFormConfig<T>) {
	let data = $state<T | null>(null);
	let loading = $state(true);
	let saving = $state(false);
	let testing = $state(false);
	let message = $state('');
	let messageType = $state<'success' | 'error'>('success');
	let testResult = $state<unknown>(null);
	let wasAlreadyEnabled = $state(false);
	let wasAlreadySecondaryEnabled = $state(false);
	let clearTimer: ReturnType<typeof setTimeout> | null = null;

	function clearMessage() {
		message = '';
		messageType = 'success';
	}

	function showMessage(msg: string, type: 'success' | 'error' = 'success', autoClear = true) {
		message = msg;
		messageType = type;
		if (clearTimer) clearTimeout(clearTimer);
		clearTimer = null;
		if (autoClear && type === 'success') {
			clearTimer = setTimeout(clearMessage, 5000);
		}
	}

	async function refreshIntegrationStatus() {
		try {
			const status = await api.global.v3.GET(HOME_ENDPOINTS.integrationStatus());
			if (status) integrationStore.setStatus(status);
		} catch {
			/* sidebar will refresh on next page load */
		}
	}

	async function load() {
		loading = true;
		message = '';
		try {
			const result = await api.global.get<T>(config.loadEndpoint);
			data = result ?? config.defaultValue ?? null;
			if (config.enabledField && data) {
				wasAlreadyEnabled = Boolean(data[config.enabledField]);
			}
			if (config.secondaryEnabledField && data) {
				wasAlreadySecondaryEnabled = Boolean(data[config.secondaryEnabledField]);
			}
		} catch (e) {
			if (!isAbortError(e)) {
				showMessage("Couldn't load your settings", 'error', false);
			}
			if (config.defaultValue) data = { ...config.defaultValue };
		} finally {
			loading = false;
		}
	}

	/** Returns true on success, false on failure. */
	async function save(): Promise<boolean> {
		if (!data) return false;
		saving = true;
		message = '';
		try {
			const result = await api.global.put<T>(config.saveEndpoint, data);
			if (result) data = result;
			if (config.enabledField && data) {
				wasAlreadyEnabled = Boolean(data[config.enabledField]);
			}
			if (config.secondaryEnabledField && data) {
				wasAlreadySecondaryEnabled = Boolean(data[config.secondaryEnabledField]);
			}
			showMessage('Settings saved');
			if (config.refreshIntegration) await refreshIntegrationStatus();
			if (config.afterSave) await config.afterSave(data!);
			return true;
		} catch (e) {
			if (!isAbortError(e)) {
				const msg = e instanceof ApiError ? e.message : "Couldn't save your settings";
				showMessage(msg, 'error', false);
			}
			return false;
		} finally {
			saving = false;
		}
	}

	async function test() {
		if (!data || !config.testEndpoint) return;
		testing = true;
		testResult = null;
		try {
			const result = await api.global.post(config.testEndpoint, data);
			testResult = result;
			if (config.afterTest) config.afterTest(result);
		} catch (e) {
			if (!isAbortError(e)) {
				const msg = e instanceof ApiError ? e.message : "Couldn't test the connection";
				testResult = { success: false, valid: false, message: msg };
			}
		} finally {
			testing = false;
		}
	}

	function cleanup() {
		if (clearTimer) {
			clearTimeout(clearTimer);
			clearTimer = null;
		}
	}

	return {
		get data() {
			return data;
		},
		set data(v: T | null) {
			data = v;
		},
		get loading() {
			return loading;
		},
		get saving() {
			return saving;
		},
		get testing() {
			return testing;
		},
		get message() {
			return message;
		},
		get messageType() {
			return messageType;
		},
		get testResult() {
			return testResult;
		},
		set testResult(v: unknown) {
			testResult = v;
		},
		get wasAlreadyEnabled() {
			return wasAlreadyEnabled;
		},
		get wasAlreadySecondaryEnabled() {
			return wasAlreadySecondaryEnabled;
		},
		load,
		save,
		test,
		showMessage,
		clearMessage,
		cleanup
	};
}
