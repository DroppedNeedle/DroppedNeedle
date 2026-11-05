<!--
	The first live download, as the Downloads page's hero: a paper card with
	the turning record (its acid rim is the progress), the cover, the big
	title, the transfer's progress, the source panel and Cancel.
-->
<script lang="ts">
	import { X } from 'lucide-svelte';

	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { createCancelDownloadMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import { createDownloadStream } from '$lib/features/downloads/queries/DownloadSSE.svelte';
	import { canCancel, derivedDownloadStatus } from '$lib/features/downloads/queries/downloadStatus';
	import type { DownloadTask } from '$lib/features/downloads/queries/types';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import Button from '$lib/ui/Button.svelte';
	import PaperPanel from '$lib/ui/PaperPanel.svelte';

	import DownloadProgressBar from './DownloadProgressBar.svelte';
	import DownloadSourceStatus from './DownloadSourceStatus.svelte';
	import DownloadStatusBadge from './DownloadStatusBadge.svelte';
	import VinylProgress from './VinylProgress.svelte';

	let { task }: { task: DownloadTask } = $props();

	const stream = createDownloadStream();
	const cancel = createCancelDownloadMutation();
	$effect(() => {
		if (task.status === 'downloading' || task.status === 'processing') {
			stream.start(task.id);
			return () => stream.stop();
		}
		stream.stop();
	});

	const derivedStatus = $derived(derivedDownloadStatus(task));
	const isSearchingState = $derived(derivedStatus === 'searching');
	const progress = $derived(stream.state.progress);
	const livePct = $derived(progress?.progress_percent ?? task.progress_percent);
	const showBar = $derived(task.status === 'downloading' || task.status === 'processing');
	const showCancel = $derived(canCancel(task));
	const hasSource = $derived(
		!!(stream.state.source?.source ?? task.source) && !task.held_for_review
	);
</script>

<section id="download-{task.id}" class="scroll-mt-24" aria-label="Now pressing">
	<PaperPanel>
		<div class="hero">
			<div class="hero__art">
				<VinylProgress
					percent={livePct}
					spinning={task.status === 'downloading'}
					indeterminate={isSearchingState}
					size={144}
				/>
				<div class="hero__cover">
					<AlbumImage
						mbid={task.release_group_mbid}
						alt={task.album_title}
						size="full"
						requestSize={250}
						rounded="none"
						className="h-full w-full"
					/>
				</div>
			</div>

			<div class="hero__main">
				<div class="hero__heading">
					<h3 class="hero__title">
						<a href={albumHref(task.release_group_mbid)} class="hero__link">{task.album_title}</a>
					</h3>
					<DownloadStatusBadge {task} />
				</div>
				<p class="hero__by">
					{#if task.artist_mbid}
						<a href={artistHref(task.artist_mbid)} class="hero__artist">{task.artist_name}</a>
					{:else}
						{task.artist_name}
					{/if}
					{#if task.year}<span class="hero__dot mr-1.5 ml-0.5">·</span>{task.year}{/if}
					{#if task.download_type === 'track' && task.track_title}
						<span class="hero__dot mr-1.5 ml-0.5">·</span>{task.track_title}
					{/if}
				</p>
				{#if showBar}
					<div class="hero__bar">
						<DownloadProgressBar
							percent={livePct}
							bytesDownloaded={progress?.bytes_downloaded ?? task.downloaded_bytes}
							bytesTotal={progress?.bytes_total ?? task.total_size_bytes ?? 0}
							filesCompleted={progress?.files_completed ?? task.files_completed}
							filesTotal={progress?.files_total ?? task.files_total}
						/>
					</div>
				{/if}
			</div>

			{#if hasSource}
				<div class="hero__source">
					<DownloadSourceStatus
						{task}
						live={stream.state.source}
						bytesDownloaded={progress?.bytes_downloaded ?? task.downloaded_bytes}
						panel
					/>
				</div>
			{/if}

			{#if showCancel}
				<div class="hero__actions">
					<Button
						variant="paper"
						size="display-sm"
						icon={X}
						onclick={() => cancel.mutate(task.id)}
						disabled={cancel.isPending}
						aria-label="Cancel download"
					>
						Cancel
					</Button>
				</div>
			{/if}
		</div>
	</PaperPanel>
</section>

<style>
	.hero {
		display: flex;
		align-items: center;
		gap: 1.5rem 2rem;
		padding: 1rem 1.75rem 1rem 1.25rem;
	}

	.hero__main {
		flex: 1 1 0;
		min-width: 0;
	}

	.hero__art {
		flex: none;
		display: flex;
		align-items: center;
		gap: 1.75rem;
	}
	.hero__cover {
		width: 8.75rem;
		height: 8.75rem;
		flex: none;
		overflow: hidden;
		border-radius: 0.25rem;
		box-shadow: 0 8px 18px -12px rgb(0 0 0 / 0.6);
	}

	.hero__heading {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 1.25rem;
	}
	.hero__title {
		min-width: 0;
		font-family: var(--font-display);
		font-size: 2.75rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		font-synthesis: none;
		overflow-wrap: anywhere;
		color: var(--dn-title);
	}
	.hero__link {
		border-radius: 0.25rem;
	}
	.hero__link:hover {
		text-decoration: underline;
		text-decoration-thickness: 3px;
		text-underline-offset: 4px;
	}

	.hero__by {
		margin-top: 0.625rem;
		font-family: var(--font-mono);
		font-size: 1rem;
	}
	.hero__artist {
		text-decoration: underline;
		text-underline-offset: 3px;
	}
	.hero__artist:hover {
		color: var(--color-primary);
	}
	.hero__dot {
		color: var(--dn-muted);
	}

	.hero__bar {
		margin-top: 1rem;
		max-width: 32rem;
	}

	.hero__source {
		flex: 0 0 17rem;
	}
	.hero__source,
	.hero__actions {
		align-self: stretch;
		display: flex;
		align-items: center;
		padding-left: 2rem;
		border-left: 1px solid var(--dn-paper-border);
	}

	@media (max-width: 1279px) {
		.hero {
			flex-wrap: wrap;
		}
		.hero__main {
			flex-basis: 16rem;
		}
		.hero__source {
			flex: 1 1 100%;
			padding: 1rem 0 0;
			border-left: 0;
			border-top: 1px solid var(--dn-paper-border);
		}
		.hero__actions {
			flex: 1 1 100%;
			justify-content: flex-end;
			padding: 0;
			border-left: 0;
		}
	}

	@media (max-width: 767px) {
		.hero {
			flex-direction: column;
			align-items: stretch;
			gap: 1rem;
			padding: 1rem;
		}
		.hero__main {
			flex-basis: auto;
		}
		.hero__art {
			gap: 1rem;
		}
		.hero__art :global(.vinyl) {
			--vinyl-size: 6.5rem !important;
		}
		.hero__cover {
			width: 6.5rem;
			height: 6.5rem;
		}
		.hero__title {
			font-size: 2rem;
		}
		.hero__by {
			margin-top: 0.375rem;
			font-size: 0.875rem;
		}
		.hero__actions {
			justify-content: stretch;
		}
		.hero__actions :global(.btn) {
			width: 100%;
		}
	}
</style>
