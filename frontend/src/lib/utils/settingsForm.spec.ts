import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

const { mockApiGet, mockApiPut, mockApiPost, mockSetStatus } = vi.hoisted(() => ({
	mockApiGet: vi.fn(),
	mockApiPut: vi.fn(),
	mockApiPost: vi.fn(),
	mockSetStatus: vi.fn()
}));

vi.mock('$lib/api/client', () => {
	class ApiError extends Error {
		status: number;
		code: string;
		details: unknown;
		constructor(status: number, code: string, message: string, details?: unknown) {
			super(message);
			this.name = 'ApiError';
			this.status = status;
			this.code = code;
			this.details = details;
		}
	}
	return {
		api: {
			global: { get: mockApiGet, put: mockApiPut, post: mockApiPost, v3: { GET: mockApiGet } },
			get: mockApiGet,
			put: mockApiPut,
			post: mockApiPost
		},
		ApiError
	};
});

vi.mock('$lib/utils/errorHandling', () => ({
	isAbortError: (e: unknown) =>
		e instanceof DOMException && (e as DOMException).name === 'AbortError'
}));

vi.mock('$lib/stores/integration', () => ({
	integrationStore: { setStatus: mockSetStatus }
}));

import { createSettingsForm } from './settingsForm.svelte';
import type { V3Url } from '$lib/api/v3/endpoint';

// Plain test URLs; the form only passes them through to the client.
const url = (path: string) => path as V3Url;

interface TestSettings {
	url: string;
	enabled: boolean;
}

const defaultConfig = {
	loadEndpoint: url('/api/v3/settings/test'),
	saveEndpoint: url('/api/v3/settings/test')
};

describe('createSettingsForm', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		vi.clearAllMocks();
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	describe('load', () => {
		it('fetches data and sets state on success', async () => {
			const data = { url: 'http://test', enabled: true };
			mockApiGet.mockResolvedValueOnce(data);
			const form = createSettingsForm<TestSettings>(defaultConfig);
			await form.load();

			expect(mockApiGet).toHaveBeenCalledWith('/api/v3/settings/test');
			expect(form.data).toEqual(data);
			expect(form.loading).toBe(false);
			expect(form.message).toBe('');
			form.cleanup();
		});

		it('uses defaultValue on load failure', async () => {
			const defaultValue = { url: '', enabled: false };
			mockApiGet.mockRejectedValueOnce(new Error('fail'));
			const form = createSettingsForm<TestSettings>({
				...defaultConfig,
				defaultValue
			});
			await form.load();

			expect(form.data).toEqual(defaultValue);
			form.cleanup();
		});
	});

	describe('save', () => {
		it('PUTs data and returns true on success', async () => {
			const data = { url: 'http://test', enabled: true };
			mockApiGet.mockResolvedValueOnce(data);
			mockApiPut.mockResolvedValueOnce(data);

			const form = createSettingsForm<TestSettings>(defaultConfig);
			await form.load();
			const result = await form.save();

			expect(mockApiPut).toHaveBeenCalledWith('/api/v3/settings/test', data);
			expect(result).toBe(true);
			expect(form.message).toBe('Settings saved');
			expect(form.messageType).toBe('success');
			form.cleanup();
		});

		it('uses ApiError message on failure', async () => {
			const { ApiError } = await import('$lib/api/client');
			mockApiGet.mockResolvedValueOnce({ url: '', enabled: false });
			mockApiPut.mockRejectedValueOnce(new ApiError(400, 'VALIDATION', 'Invalid URL format'));

			const form = createSettingsForm<TestSettings>(defaultConfig);
			await form.load();
			const result = await form.save();

			expect(result).toBe(false);
			expect(form.message).toBe('Invalid URL format');
			form.cleanup();
		});
	});

	describe('test', () => {
		it('POSTs data to test endpoint and sets testResult', async () => {
			const data = { url: 'http://test', enabled: true };
			const testData = { success: true, message: 'Connected' };
			mockApiGet.mockResolvedValueOnce(data);
			mockApiPost.mockResolvedValueOnce(testData);

			const form = createSettingsForm<TestSettings>({
				...defaultConfig,
				testEndpoint: url('/api/v3/settings/test/verify')
			});
			await form.load();
			await form.test();

			expect(mockApiPost).toHaveBeenCalledWith('/api/v3/settings/test/verify', data);
			expect(form.testResult).toEqual(testData);
			expect(form.testing).toBe(false);
			form.cleanup();
		});

		it('sets failure testResult on error', async () => {
			mockApiGet.mockResolvedValueOnce({ url: 'http://test', enabled: true });
			mockApiPost.mockRejectedValueOnce(new Error('timeout'));

			const form = createSettingsForm<TestSettings>({
				...defaultConfig,
				testEndpoint: url('/api/v3/settings/test/verify')
			});
			await form.load();
			await form.test();

			expect(form.testResult).toEqual({
				success: false,
				valid: false,
				message: "Couldn't test the connection"
			});
			form.cleanup();
		});
	});

	describe('cleanup', () => {
		it('clears pending auto-clear timer', async () => {
			const data = { url: 'http://test', enabled: true };
			mockApiGet.mockResolvedValueOnce(data);
			mockApiPut.mockResolvedValueOnce(data);

			const form = createSettingsForm<TestSettings>(defaultConfig);
			await form.load();
			await form.save();
			expect(form.message).toBe('Settings saved');

			form.cleanup();
			vi.advanceTimersByTime(5000);
			expect(form.message).toBe('Settings saved');
		});
	});
});
