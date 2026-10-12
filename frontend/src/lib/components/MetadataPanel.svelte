<script lang="ts">
	import { X, Info, ExternalLink } from 'lucide-svelte';
	import { slide } from 'svelte/transition';
	import { getApiUrl } from '$lib/api/api-utils';
	import SafeHtml from '$lib/components/SafeHtml.svelte';

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

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			open = false;
			onclose();
		}
	}

	const musicbrainzUrl = $derived(
		musicbrainzId ? `https://musicbrainz.org/release-group/${musicbrainzId}` : ''
	);
</script>

{#if open}
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
		role="dialog"
		aria-label="Album details"
		aria-modal="true"
		tabindex="-1"
		onkeydown={handleKeydown}
		onclick={() => {
			open = false;
			onclose();
		}}
	>
		<div
			class="bg-base-200 rounded-xl shadow-xl border border-base-300 w-full max-w-md mx-4 max-h-[70vh] flex flex-col"
			transition:slide={{ duration: 200 }}
			onkeydown={(e) => e.stopPropagation()}
			onclick={(e) => e.stopPropagation()}
		>
			<div class="flex items-center justify-between px-4 py-3 border-b border-base-300">
				<div class="flex items-center gap-2">
					<Info class="h-4 w-4 text-primary" />
					<span class="font-semibold text-sm">{title}</span>
				</div>
				<button
					class="btn btn-ghost btn-sm btn-circle"
					onclick={() => {
						open = false;
						onclose();
					}}
					aria-label="Close details"
				>
					<X class="h-4 w-4" />
				</button>
			</div>

			<div class="overflow-y-auto px-4 py-4 flex-1 space-y-4">
				{#if imageUrl}
					<img
						src={getApiUrl(imageUrl)}
						alt={title}
						class="w-full rounded-lg object-cover max-h-48"
					/>
				{/if}

				{#if notes}
					<!-- Last.fm wiki text relayed by Navidrome. Anyone can edit it. -->
					<SafeHtml html={notes} class="text-sm leading-relaxed text-base-content/80" />
				{:else}
					<p class="text-center text-base-content/40 py-4">No notes available for this release.</p>
				{/if}

				{#if lastfmUrl || musicbrainzUrl}
					<div class="flex flex-wrap gap-2 pt-2">
						{#if lastfmUrl}
							<a
								href={lastfmUrl}
								target="_blank"
								rel="noopener noreferrer"
								class="btn btn-ghost btn-xs gap-1"
							>
								<ExternalLink class="h-3 w-3" />
								Last.fm
							</a>
						{/if}
						{#if musicbrainzUrl}
							<a
								href={musicbrainzUrl}
								target="_blank"
								rel="noopener noreferrer"
								class="btn btn-ghost btn-xs gap-1"
							>
								<ExternalLink class="h-3 w-3" />
								MusicBrainz
							</a>
						{/if}
					</div>
				{/if}
			</div>
		</div>
	</div>
{/if}
