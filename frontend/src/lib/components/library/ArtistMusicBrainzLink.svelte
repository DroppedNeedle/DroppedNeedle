<!--
	An artist's MusicBrainz link: the id, who set it and when. A curator can
	link the artist by hand, and clear a link someone set; the app then links
	it again from its albums where it can.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { Link2, Pencil, Unlink } from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import { outsideLink } from '$lib/components/outsideLink';
	import {
		type ArtistLink,
		clearArtistLink,
		getArtistLinkQuery,
		setArtistLink
	} from '$lib/queries/artist-link/ArtistLinkQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { artistHref } from '$lib/utils/entityRoutes';

	interface Props {
		/** The library artist's own id. */
		artistId: string;
		className?: string;
	}

	let { artistId, className = '' }: Props = $props();

	const linkQuery = getArtistLinkQuery(() => artistId);
	const link = $derived(linkQuery.data);
	const setLink = setArtistLink();
	const clearLink = clearArtistLink();

	let editing = $state(false);
	let clearing = $state(false);
	let entered = $state('');

	const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

	const byLine: Record<NonNullable<ArtistLink['set_by']>, string> = {
		you: 'Linked by you',
		person: 'Linked by a curator',
		app: 'Linked by DroppedNeedle',
		v2: 'Linked in v2'
	};

	const canClear = $derived(link?.source === 'person' || link?.source === 'v2');
	const page = $derived(
		link?.musicbrainz_artist_id
			? outsideLink(`https://musicbrainz.org/artist/${link.musicbrainz_artist_id}`)
			: undefined
	);

	function when(ms: number): string {
		return new Date(ms).toLocaleDateString();
	}

	// The refusal as one sentence, with the artists it names.
	function refusal(error: unknown): Error {
		if (!(error instanceof ApiError)) return error instanceof Error ? error : new Error();
		const details = (error.details ?? {}) as {
			action?: string;
			artists?: { name: string }[];
		};
		const names = details.artists?.map((artist) => artist.name).join(', ');
		return new Error(
			[error.message, names ? `(${names})` : '', details.action ?? ''].filter(Boolean).join(' ')
		);
	}

	function moved(before: ArtistLink, after: ArtistLink) {
		if (after.musicbrainz_artist_id === before.musicbrainz_artist_id) return;
		void goto(artistHref(after.musicbrainz_artist_id ?? after.artist_id), { replaceState: true });
	}

	async function save() {
		if (!link) return;
		const id = UUID.exec(entered)?.[0] ?? entered.trim();
		try {
			moved(link, await setLink.mutateAsync({ link, musicbrainzArtistId: id }));
		} catch (error) {
			throw refusal(error);
		}
	}

	async function clear() {
		if (!link) return;
		try {
			moved(link, await clearLink.mutateAsync(link));
		} catch (error) {
			throw refusal(error);
		}
	}

	function openEditor() {
		entered = link?.musicbrainz_artist_id ?? '';
		editing = true;
	}
</script>

{#if link}
	<div
		class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-base-content/65 {className}"
		aria-label="MusicBrainz link"
		role="group"
	>
		<Link2 class="h-4 w-4 shrink-0" aria-hidden="true" />
		{#if link.musicbrainz_artist_id && link.set_by && link.selected_at != null}
			<a
				class="link link-hover font-medium text-base-content/80"
				href={page}
				target="_blank"
				rel="noreferrer">MusicBrainz</a
			>
			<span>· {byLine[link.set_by]}, {when(link.selected_at)}</span>
		{:else}
			<span>Not linked to MusicBrainz</span>
		{/if}
		{#if authStore.isTrusted}
			<Button variant="ghost" size="xs" icon={Pencil} onclick={openEditor}>
				{link.musicbrainz_artist_id ? 'Change' : 'Link'}
			</Button>
			{#if canClear}
				<Button variant="ghost" size="xs" icon={Unlink} onclick={() => (clearing = true)}>
					Clear
				</Button>
			{/if}
		{/if}
	</div>

	<ConfirmDialog
		bind:open={editing}
		title="Link to MusicBrainz"
		confirmLabel="Link"
		pending={setLink.isPending}
		onconfirm={save}
	>
		<p class="text-sm text-base-content/70">
			DroppedNeedle keeps a link you set. It never merges artists for you.
		</p>
		<div class="mt-3">
			<TextField
				label="MusicBrainz artist ID or page address"
				bind:value={entered}
				mono
				placeholder="https://musicbrainz.org/artist/…"
			/>
		</div>
	</ConfirmDialog>

	<ConfirmDialog
		bind:open={clearing}
		title="Clear the link?"
		message="DroppedNeedle will link this artist again from its albums where it can."
		confirmLabel="Clear"
		tone="danger"
		pending={clearLink.isPending}
		onconfirm={clear}
	/>
{/if}
