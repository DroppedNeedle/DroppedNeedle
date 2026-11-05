<!--
	A track's codec as a format chip (lossless in the action colour, lossy
	plain), and unless `compact` its bitrate, channels and container.
	`converted` marks a track the server converts for this browser.
-->
<script lang="ts">
	import StatusChip from '$lib/ui/StatusChip.svelte';

	interface Props {
		codec?: string | null;
		bitrate?: number | null;
		audioChannels?: number | null;
		container?: string | null;
		compact?: boolean;
		converted?: boolean;
	}

	let {
		codec = null,
		bitrate = null,
		audioChannels = null,
		container = null,
		compact = false,
		converted = false
	}: Props = $props();

	const LOSSLESS_CODECS = ['flac', 'alac', 'wav', 'aiff', 'dsd', 'ape', 'wv', 'pcm'];

	const isLossless = $derived(codec ? LOSSLESS_CODECS.includes(codec.toLowerCase()) : false);

	const bitrateLabel = $derived.by(() => {
		if (!bitrate) return '';
		if (bitrate >= 1000) return `${(bitrate / 1000).toFixed(1)} Mbps`;
		return `${bitrate} kbps`;
	});

	const channelLabel = $derived.by(() => {
		if (!audioChannels) return '';
		if (audioChannels === 1) return 'Mono';
		if (audioChannels === 2) return 'Stereo';
		return `${audioChannels}.${audioChannels > 6 ? '1' : '0'}`;
	});

	const hasAnyInfo = $derived(!!codec || !!bitrate);
</script>

{#if hasAnyInfo}
	<span class="inline-flex flex-wrap items-center gap-1">
		{#if codec}
			<span class="inline-flex" title={isLossless ? 'Lossless' : 'Lossy'}>
				<StatusChip size="xs" tone={isLossless ? 'action' : 'neutral'}>{codec}</StatusChip>
			</span>
		{/if}
		{#if converted}
			<span class="inline-flex" title="Converted for this browser">
				<StatusChip size="xs">Converted</StatusChip>
			</span>
		{/if}

		{#if !compact}
			{#if bitrate}
				<StatusChip size="xs">{bitrateLabel}</StatusChip>
			{/if}
			{#if audioChannels}
				<StatusChip size="xs">{channelLabel}</StatusChip>
			{/if}
			{#if container && container !== codec}
				<StatusChip size="xs">{container}</StatusChip>
			{/if}
		{/if}
	</span>
{/if}
