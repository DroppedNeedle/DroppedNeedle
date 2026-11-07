// The signed-in person's appearance, from their account: the theme, the
// site's CSS and their own, and the one-time move of what this browser kept
// before both lived on the account.
import { sha256 } from '@noble/hashes/sha2.js';
import { bytesToHex } from '@noble/hashes/utils.js';
import { untrack } from 'svelte';
import { ApiError } from '$lib/api/errors';
import {
	createImportAppearanceMutation,
	createSaveAppearanceMutation,
	getMyAppearanceQuery,
	takeFetchedAppearance,
	type AppearanceImported,
	type AppearanceUpdate,
	type MyAppearance,
	type ThemeChoice
} from '$lib/features/profile/queries/appearance.svelte';
import { AppearanceQueryKeyFactory } from '$lib/queries/featureKeys';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { customCssStore } from '$lib/stores/customCss.svelte';
import { setThemeOwner, THEME_STORAGE_KEY, themeOwner, themeStore } from '$lib/stores/theme.svelte';
import { toastStore } from '$lib/stores/toast';

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
function claimThemeCopy(): void {
	const userId = authStore.user?.id;
	if (userId) setThemeOwner(ownerTag(userId));
}

// A theme save with no answer by then fails, so the saves behind it go on.
const THEME_SAVE_TIMEOUT_MS = 15_000;

type ThemeSend = (update: AppearanceUpdate) => Promise<unknown>;

/** A theme choice to store on the account, and the account it was made for. */
interface ThemeSave {
	theme: ThemeChoice;
	userId: string | undefined;
	send: ThemeSend;
}

// Theme saves reach the account one at a time, in the order they were made.
// `sending` is the save on its way; `waiting` is the newest choice made since,
// which replaces any older choice still waiting.
let sending: ThemeSave | null = null;
let waiting: ThemeSave | null = null;

/** Whether a theme choice of this account is on its way or waiting. */
function themeSavePending(userId: string): boolean {
	return sending?.userId === userId || waiting?.userId === userId;
}

/** Whether the choice waiting is the same choice, for the same account, as `save`. */
function isWaiting(save: ThemeSave): boolean {
	return waiting !== null && waiting.theme === save.theme && waiting.userId === save.userId;
}

async function sendWaiting(): Promise<void> {
	while (waiting) {
		const save = waiting;
		waiting = null;
		// A choice made for a different account, or for nobody, is dropped.
		if (save.userId !== authStore.user?.id) continue;
		sending = save;
		let stored = false;
		try {
			await save.send({ theme: save.theme });
			stored = true;
		} catch {
			toastStore.show({ message: 'Theme not saved to your account.', type: 'error' });
		}
		sending = null;
		// The account holds this choice already, so a copy of it waiting is not sent again.
		if (stored && isWaiting(save)) waiting = null;
	}
}

/**
 * The theme saver for the component being set up. `saveTheme(next)` shows
 * `next` at once, claims this browser's copy and saves `next` to the account,
 * after any save made before it. A failed save shows an error and keeps
 * `next` on screen.
 */
export function createThemeSaver(): (next: ThemeChoice) => void {
	const save = createSaveAppearanceMutation(THEME_SAVE_TIMEOUT_MS);
	return (next) => {
		themeStore.set(next);
		claimThemeCopy();
		waiting = {
			theme: next,
			userId: authStore.user?.id,
			send: (update) => save.mutateAsync(update)
		};
		if (!sending) void sendWaiting();
	};
}

// The account's own theme wins. Otherwise an unclaimed copy becomes this
// account's, and another account's copy is replaced by the default, claimed
// for this account. Nothing is saved for either. An account answer does not
// touch the screen while this person's theme save is pending: that save decides it.
function applyTheme(theme: ThemeChoice | null, tag: string, userId: string): void {
	if (themeSavePending(userId)) return;
	if (theme) {
		themeStore.set(theme);
		setThemeOwner(tag);
	} else if (!ownsBrowserTheme(tag)) {
		themeStore.set('dark');
		setThemeOwner(tag);
	} else if (readLocalTheme()) {
		setThemeOwner(tag);
	}
}

/**
 * The account and this browser's theme follow the signed-in user, and the
 * site's and their own CSS apply. The first answer after mount moves what this
 * browser kept up to the account, once: the theme when the account has no row
 * yet, and the sheet unless the account has one of its own. Call once from the
 * signed-in shell's script.
 */
export function applyAppearance(): void {
	const query = getMyAppearanceQuery();
	const importer = createImportAppearanceMutation();
	const sheetSaver = createSaveAppearanceMutation();
	const mountedAt = Date.now();
	// The users this page already tried to move, so a refusal is not retried on every answer.
	const moveTried: string[] = [];

	/** Puts the browser's sheet on the account by PUT. Its copy goes once the account holds it. */
	async function saveSheet(userId: string, text: string): Promise<void> {
		try {
			const saved = await sheetSaver.mutateAsync({ custom_css: { text } });
			if (saved.custom_css.text.trim()) removeOldCss(userId);
		} catch {
			// A refused or failed save keeps the browser's copy for the next load.
		}
	}

	/**
	 * Moves what this browser kept up to the account. The theme goes only into an
	 * account with no row yet. While there is no row, the sheet goes in with the
	 * import; once there is one, it goes by PUT. A copy is removed only after the
	 * account answers holding its sheet, and a sheet the account has of its own wins.
	 */
	async function moveFromBrowser(
		userId: string,
		answer: MyAppearance,
		browserTheme: ThemeChoice | null
	): Promise<void> {
		const theme = answer.stored ? null : browserTheme;
		const text = readOldCss(userId);
		const sheet = text.trim() ? text : null;
		// The account's own sheet wins: the copy goes, and nothing is uploaded.
		if (sheet && answer.custom_css.text.trim()) {
			removeOldCss(userId);
			return;
		}
		if (!answer.stored) {
			if (!theme && !sheet) return;
			const update: AppearanceUpdate = {};
			if (theme) update.theme = theme;
			if (sheet) update.custom_css = { text };
			let imported: AppearanceImported;
			try {
				imported = await importer.mutateAsync(update);
			} catch (error) {
				// A refused sheet stays in this browser; the theme still moves on its own.
				if (isRefusal(error) && theme && sheet) {
					await importer.mutateAsync({ theme }).catch(() => undefined);
				}
				return;
			}
			if (!sheet) return;
			if (imported.custom_css.text.trim()) {
				removeOldCss(userId);
				return;
			}
			// Written but still blank: keep the copy. Otherwise the account has a row
			// with no sheet, and the sheet goes by PUT.
			if (imported.imported) return;
		}
		if (sheet) await saveSheet(userId, text);
	}

	$effect(() => {
		const userId = authStore.user?.id;
		if (!userId) return;
		// A real change of user takes the last one's sheets off.
		return () => customCssStore.hide();
	});

	// Once the user is known, another account's copy is not shown as theirs,
	// before the account has answered.
	$effect(() => {
		const userId = authStore.user?.id;
		if (!userId) return;
		untrack(() => {
			if (!ownsBrowserTheme(ownerTag(userId))) themeStore.show('dark');
		});
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
			// Read before the theme is applied, since applying replaces another
			// account's copy. That copy is never moved to this account.
			const browserTheme = ownsBrowserTheme(tag) ? readLocalTheme() : null;
			applyTheme(appearance.theme ?? null, tag, userId);
			const fetched = takeFetchedAppearance();
			if (
				fetched &&
				fetched.userId === userId &&
				fetched.fetchedAt >= mountedAt &&
				!moveTried.includes(userId)
			) {
				moveTried.push(userId);
				void moveFromBrowser(userId, fetched.answer, browserTheme);
			}
		});
	});
}
