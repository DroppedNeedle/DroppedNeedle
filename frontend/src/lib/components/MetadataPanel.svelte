<script lang="ts">
	import { ExternalLink } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import SafeHtml from '$lib/components/SafeHtml.svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';

	interface Props {
		open: boolean;
		title: string;
		notes?: string;
		imageUrl?: string;
		lastfmUrl?: string;
		musicbrainzId?: string;
		onclose: () => void;
	}

	let {
		open = $bindable(),
		title,
		notes = '',
		imageUrl = '',
		lastfmUrl = '',
		musicbrainzId = '',
		onclose
	}: Props = $props();

	// Relayed by the connected media server, so it is checked before it is linked.
	const lastfmLink = $derived(outsideLink(lastfmUrl));
	const musicbrainzUrl = $derived(
		musicbrainzId ? `https://musicbrainz.org/release-group/${musicbrainzId}` : ''
	);
</script>

<Modal bind:open {title} closeButton {onclose}>
	<div class="space-y-4">
		{#if imageUrl}
			<AlbumImage customUrl={imageUrl} alt={title} size="full" className="h-48 w-full" />
		{/if}

		{#if notes}
			<!-- Last.fm wiki text relayed by Navidrome. Anyone can edit it. -->
			<SafeHtml html={notes} class="text-sm leading-relaxed text-base-content/80" />
		{:else}
			<p class="text-center text-base-content/40 py-4">No notes available for this release.</p>
		{/if}

		{#if lastfmLink || musicbrainzUrl}
			<div class="flex flex-wrap gap-2 pt-2">
				{#if lastfmLink}
					<Button
						variant="ghost"
						size="xs"
						icon={ExternalLink}
						href={lastfmLink}
						target="_blank"
						rel="noopener noreferrer"
					>
						Last.fm
					</Button>
				{/if}
				{#if musicbrainzUrl}
					<Button
						variant="ghost"
						size="xs"
						icon={ExternalLink}
						href={musicbrainzUrl}
						target="_blank"
						rel="noopener noreferrer"
					>
						MusicBrainz
					</Button>
				{/if}
			</div>
		{/if}
	</div>
</Modal>
