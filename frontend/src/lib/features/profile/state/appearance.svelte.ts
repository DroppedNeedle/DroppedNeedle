// The signed-in person's appearance, from their account: the theme, the
// site's CSS and their own, and the one-time move of what this browser kept
// before both lived on the account.
import { sha256 } from '@noble/hashes/sha2.js';
import { bytesToHex } from '@noble/hashes/utils.js';
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
import { setThemeOwner, THEME_STORAGE_KEY, themeOwner, themeStore } from '$lib/stores/theme.svelte';

// The key the browser-only version kept a person's CSS under, read once to
// move it to their account.
const OLD_KEY_PREFIX = 'droppedneedle_custom_css:';
const THEMES: readonly ThemeChoice[] = ['system', 'light', 'dark', 'midnight'];
// The owner tag is SHA-256 of this prefix and the account id, so the id itself
// is never kept in the browser.
const OWNER_TAG_PREFIX = 'droppedneedle theme owner:';

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

function ownerTag(userId: string): string {
	return bytesToHex(sha256(new TextEncoder().encode(OWNER_TAG_PREFIX + userId)));
}

// Whether this browser's theme copy belongs to the account with this tag, or
// no account has claimed it yet.
function ownsBrowserTheme(tag: string): boolean {
	const owner = themeOwner();
	return owner === null || owner === tag;
}

function isRefusal(error: unknown): boolean {
	return error instanceof ApiError && (error.status === 400 || error.status === 403);
}

/** Claim this browser's theme copy for the signed-in person. */
export function claimThemeCopy(): void {
	const userId = authStore.user?.id;
	if (userId) setThemeOwner(ownerTag(userId));
}

// The account's own theme wins. Otherwise an unclaimed copy becomes this
// account's, and another account's copy gives way to the default.
function applyTheme(theme: ThemeChoice | null, tag: string): void {
	if (theme) {
		themeStore.set(theme);
		setThemeOwner(tag);
	} else if (!ownsBrowserTheme(tag)) {
		// Another account's copy: show the default and save nothing.
		themeStore.show('dark');
	} else if (readLocalTheme()) {
		setThemeOwner(tag);
	}
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

	async function moveFromBrowser(userId: string, tag: string): Promise<void> {
		// Another account's theme is not this account's to move.
		const theme = ownsBrowserTheme(tag) ? readLocalTheme() : null;
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
			const tag = ownerTag(userId);
			applyTheme(appearance.theme ?? null, tag);
			const fetched = takeFetchedAppearance();
			if (
				fetched &&
				fetched.userId === userId &&
				fetched.fetchedAt >= mountedAt &&
				!fetched.answer.stored &&
				!moveTried.includes(userId)
			) {
				moveTried.push(userId);
				void moveFromBrowser(userId, tag);
			}
		});
	});
}
