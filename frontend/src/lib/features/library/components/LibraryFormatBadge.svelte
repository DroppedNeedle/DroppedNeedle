<script lang="ts">
	import StatusChip from '$lib/ui/StatusChip.svelte';

	// Shows a track's format or an album's primary format (its most common
	// indexed-track codec, plus 'mixed' defensively); lossy codecs include bitrate.
	interface Props {
		format: string | null | undefined;
		/** Track bitrate or album primary-format rounded average bitrate, kbps. */
		bitrate?: number | null;
		size?: 'badge-xs' | 'badge-sm';
	}

	let { format, bitrate = null, size = 'badge-sm' }: Props = $props();

	type Tone = 'action' | 'neutral';

	const config = $derived.by((): { label: string; tone: Tone } | null => {
		const f = (format ?? '').toLowerCase();
		if (!f) return null;
		const roundedBitrate = bitrate != null && Number.isFinite(bitrate) ? Math.round(bitrate) : 0;
		const withBitrate = (label: string) =>
			roundedBitrate > 0 ? `${label} ${roundedBitrate}` : label;
		if (f === 'mixed') return { label: 'MIXED', tone: 'neutral' };
		if (f === 'flac' || f === 'wav' || f === 'alac')
			return { label: f.toUpperCase(), tone: 'action' };
		if (f === 'mp3') return { label: withBitrate('MP3'), tone: 'neutral' };
		if (f === 'ogg' || f === 'oga') return { label: withBitrate('OGG'), tone: 'neutral' };
		if (f === 'opus') return { label: withBitrate('OPUS'), tone: 'neutral' };
		if (f === 'm4a' || f === 'aac' || f === 'mp4')
			return { label: withBitrate('M4A'), tone: 'neutral' };
		return { label: f.toUpperCase(), tone: 'neutral' };
	});
</script>

{#if config}
	<StatusChip tone={config.tone} size={size === 'badge-xs' ? 'xs' : 'sm'}>{config.label}</StatusChip
	>
{/if}
