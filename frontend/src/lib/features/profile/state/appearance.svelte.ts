// The signed-in person's appearance, from their account: the theme, the
// site's CSS and their own, and the one-time move of what this browser kept
// before both lived on the account.
import { untrack } from 'svelte';
import { ApiError } from '$lib/api/errors';
import {
	createImportAppearanceMutation,
	getMyAppearanceQuery,
	takeFetchedAppearance,
	type AppearanceUpdate,
	type MyAppearance,
	type ThemeChoice
} from '$lib/features/profile/queries/appearance.svelte';
import { AppearanceQueryKeyFactory } from '$lib/queries/featureKeys';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { customCssStore } from '$lib/stores/customCss.svelte';
import { THEME_STORAGE_KEY, themeStore } from '$lib/stores/theme.svelte';

// The key the browser-only version kept a person's CSS under, read once to
// move it to their account.
const OLD_KEY_PREFIX = 'droppedneedle_custom_css:';
const THEMES: readonly ThemeChoice[] = ['system', 'light', 'dark', 'midnight'];

function readOldCss(userId: string): string {
	try {
		return localStorage.getItem(`${OLD_KEY_PREFIX}${userId}`) ?? '';
	} catch {
		return '';
	}
}

function removeOldCss(userId: string): void {
	try {
		localStorage.removeItem(`${OLD_KEY_PREFIX}${userId}`);
	} catch {
		// Storage refused: the account holds the sheet, so a leftover copy is harmless.
	}
}

function readLocalTheme(): ThemeChoice | null {
	try {
		const raw = localStorage.getItem(THEME_STORAGE_KEY);
		return THEMES.find((theme) => theme === raw) ?? null;
	} catch {
		return null;
	}
}

function isRefusal(error: unknown): boolean {
	return error instanceof ApiError && (error.status === 400 || error.status === 403);
}

/**
 * The account and this browser's theme follow the signed-in user, and the
 * site's and their own CSS apply. The first time an account answers with
 * nothing stored, the theme and CSS this browser kept move up to it. Call
 * once from the signed-in shell's script.
 */
export function applyAppearance(): void {
	const query = getMyAppearanceQuery();
	const importer = createImportAppearanceMutation();
	const mountedAt = Date.now();
	// The users this page already tried to move, so a refusal is not retried on every answer.
	const moveTried: string[] = [];

	async function moveFromBrowser(userId: string): Promise<void> {
		const theme = readLocalTheme();
		const text = readOldCss(userId);
		if (!theme && !text.trim()) return;
		const update: AppearanceUpdate = {};
		if (theme) update.theme = theme;
		if (text.trim()) update.custom_css = { text };
		try {
			await importer.mutateAsync(update);
			removeOldCss(userId);
		} catch (error) {
			// A refused sheet stays in this browser; the theme still moves on its own.
			if (!isRefusal(error) || !theme || !update.custom_css) return;
			await importer.mutateAsync({ theme }).catch(() => undefined);
		}
	}

	$effect(() => {
		const userId = authStore.user?.id;
		if (!userId) return;
		// A real change of user takes the last one's sheets off.
		return () => customCssStore.hide();
	});

	$effect(() => {
		const userId = authStore.user?.id;
		// Read for their changes only: the answer is read below by the user's
		// own key, so a moment where the query still holds the last user's
		// answer never applies it.
		void query.data;
		void query.dataUpdatedAt;
		if (!userId) return;
		untrack(() => {
			const appearance = queryClient.getQueryData<MyAppearance>(
				AppearanceQueryKeyFactory.mine(userId)
			);
			if (!appearance) return;
			customCssStore.showFor(userId, appearance);
			if (appearance.theme) themeStore.set(appearance.theme);
			const fetched = takeFetchedAppearance();
			if (
				fetched &&
				fetched.userId === userId &&
				fetched.fetchedAt >= mountedAt &&
				!fetched.answer.stored &&
				!moveTried.includes(userId)
			) {
				moveTried.push(userId);
				void moveFromBrowser(userId);
			}
		});
	});
}
