<!--
	A track's codec as a format chip (lossless in the action colour, lossy
	plain), plus its bitrate, sample rate, channels and container when not
	`compact`. In `compact` mode those details move into the codec chip's
	hover title. `converted` marks a track the server converts for this
	browser.
-->
<script lang="ts">
	import StatusChip from '$lib/ui/StatusChip.svelte';

	interface Props {
		codec?: string | null;
		bitrate?: number | null;
		sampleRate?: number | null;
		audioChannels?: number | null;
		container?: string | null;
		compact?: boolean;
		converted?: boolean;
	}

	let {
		codec = null,
		bitrate = null,
		sampleRate = null,
		audioChannels = null,
		container = null,
		compact = false,
		converted = false
	}: Props = $props();

	const LOSSLESS_CODECS = ['flac', 'alac', 'wav', 'aiff', 'dsd', 'ape', 'wv', 'pcm'];

	const isLossless = $derived(codec ? LOSSLESS_CODECS.includes(codec.toLowerCase()) : false);

	const bitrateLabel = $derived.by(() => {
		if (!bitrate || bitrate <= 0) return '';
		if (bitrate >= 1000) return `${(bitrate / 1000).toFixed(1)} Mbps`;
		return `${bitrate} kbps`;
	});

	const channelLabel = $derived.by(() => {
		if (!audioChannels || audioChannels <= 0) return '';
		if (audioChannels === 1) return 'Mono';
		if (audioChannels === 2) return 'Stereo';
		return `${audioChannels}.${audioChannels > 6 ? '1' : '0'}`;
	});

	const sampleRateLabel = $derived.by(() => {
		if (!sampleRate || sampleRate <= 0) return '';
		const khz = sampleRate / 1000;
		return Number.isInteger(khz) ? `${khz} kHz` : `${khz.toFixed(1)} kHz`;
	});

	/** The values the compact chip has no room for, as its hover title. */
	const detailTitle = $derived(
		[bitrateLabel, sampleRateLabel, channelLabel].filter(Boolean).join(' · ')
	);

	const codecTitle = $derived.by(() => {
		const kind = isLossless ? 'Lossless' : 'Lossy';
		return compact && detailTitle ? `${kind} · ${detailTitle}` : kind;
	});

	const hasAnyInfo = $derived(
		compact
			? !!codec || converted
			: !!codec ||
					converted ||
					!!bitrateLabel ||
					!!sampleRateLabel ||
					!!channelLabel ||
					(!!container && container !== codec)
	);
</script>

{#if hasAnyInfo}
	<span class="inline-flex flex-wrap items-center gap-1">
		{#if codec}
			<span class="inline-flex" title={codecTitle}>
				<StatusChip size="xs" tone={isLossless ? 'action' : 'neutral'}>{codec}</StatusChip>
			</span>
		{/if}
		{#if converted}
			<span class="inline-flex" title="Converted for this browser">
				<StatusChip size="xs">Converted</StatusChip>
			</span>
		{/if}

		{#if !compact}
			{#if bitrateLabel}
				<StatusChip size="xs">{bitrateLabel}</StatusChip>
			{/if}
			{#if sampleRateLabel}
				<StatusChip size="xs">{sampleRateLabel}</StatusChip>
			{/if}
			{#if channelLabel}
				<StatusChip size="xs">{channelLabel}</StatusChip>
			{/if}
			{#if container && container !== codec}
				<StatusChip size="xs">{container}</StatusChip>
			{/if}
		{/if}
	</span>
{/if}
