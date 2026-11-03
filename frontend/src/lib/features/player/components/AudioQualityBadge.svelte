<script lang="ts">
	interface Props {
		codec?: string | null;
		bitrate?: number | null;
		audioChannels?: number | null;
		container?: string | null;
		compact?: boolean;
	}

	let {
		codec = null,
		bitrate = null,
		audioChannels = null,
		container = null,
		compact = false
	}: Props = $props();

	const LOSSLESS_CODECS = ['flac', 'alac', 'wav', 'aiff', 'dsd', 'ape', 'wv', 'pcm'];

	const isLossless = $derived(codec ? LOSSLESS_CODECS.includes(codec.toLowerCase()) : false);

	const bitrateLabel = $derived(() => {
		if (!bitrate) return '';
		if (bitrate >= 1000) return `${(bitrate / 1000).toFixed(1)} Mbps`;
		return `${bitrate} kbps`;
	});

	const channelLabel = $derived(() => {
		if (!audioChannels) return '';
		if (audioChannels === 1) return 'Mono';
		if (audioChannels === 2) return 'Stereo';
		return `${audioChannels}.${audioChannels > 6 ? '1' : '0'}`;
	});

	const hasAnyInfo = $derived(!!codec || !!bitrate);
</script>

{#if hasAnyInfo}
	<div class="inline-flex items-center gap-1 flex-wrap">
		{#if codec}
			<span
				class="badge badge-xs uppercase font-mono {isLossless ? 'badge-success' : 'badge-ghost'}"
				title={isLossless ? 'Lossless' : 'Lossy'}
			>
				{codec}
			</span>
		{/if}

		{#if !compact}
			{#if bitrate}
				<span class="badge badge-xs badge-ghost font-mono">{bitrateLabel()}</span>
			{/if}
			{#if audioChannels}
				<span class="badge badge-xs badge-ghost">{channelLabel()}</span>
			{/if}
			{#if container && container !== codec}
				<span class="badge badge-xs badge-ghost uppercase font-mono">{container}</span>
			{/if}
		{/if}
	</div>
{/if}
