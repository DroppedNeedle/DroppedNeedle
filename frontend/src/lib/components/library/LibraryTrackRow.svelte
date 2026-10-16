<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import { Pencil } from 'lucide-svelte';
	import AudioQualityBadge from '$lib/components/AudioQualityBadge.svelte';
	import TagEditor from './TagEditor.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { formatBytes, formatDurationSec } from '$lib/utils/formatting';
	import type { LibraryFileMeta } from '$lib/types';

	interface Props {
		meta: LibraryFileMeta;
	}

	let { meta }: Props = $props();

	let editing = $state(false);
</script>

<div class="bg-base-100/60 px-4 py-3 text-xs">
	<div class="mb-2 flex flex-wrap items-center gap-2">
		<AudioQualityBadge codec={meta.format} bitrate={meta.bit_rate} />
		{#if meta.bit_depth && meta.sample_rate}
			<span class="badge badge-ghost badge-xs font-mono">
				{meta.bit_depth}/{Math.round(meta.sample_rate / 1000)}k
			</span>
		{/if}
		{#if authStore.isAdmin}
			<Button
				variant="ghost"
				size="xs"
				icon={Pencil}
				class="ml-auto"
				onclick={() => (editing = true)}>Edit tags</Button
			>
		{/if}
	</div>

	<dl class="grid grid-cols-1 gap-x-4 gap-y-1 sm:grid-cols-2">
		<div>
			<dt class="inline text-base-content/50">Duration:</dt>
			{formatDurationSec(meta.duration_seconds)}
		</div>
		<div>
			<dt class="inline text-base-content/50">Size:</dt>
			{formatBytes(meta.file_size_bytes)}
		</div>
		{#if meta.bit_rate}
			<div>
				<dt class="inline text-base-content/50">Bitrate:</dt>
				{meta.bit_rate} kbps
			</div>
		{/if}
		{#if meta.sample_rate}
			<div>
				<dt class="inline text-base-content/50">Sample rate:</dt>
				{meta.sample_rate} Hz
			</div>
		{/if}
		{#if meta.musicbrainz_recording_id}
			<div class="truncate sm:col-span-2">
				<dt class="inline text-base-content/50">Recording MBID:</dt>
				<span class="font-mono">{meta.musicbrainz_recording_id}</span>
			</div>
		{/if}
		<div class="break-all sm:col-span-2">
			<dt class="inline text-base-content/50">Local track ID:</dt>
			<span class="font-mono">{meta.id}</span>
		</div>
	</dl>
</div>

{#if authStore.isAdmin}
	<TagEditor track={meta} bind:open={editing} />
{/if}
