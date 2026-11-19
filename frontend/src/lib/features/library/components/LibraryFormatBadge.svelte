<script lang="ts">
	import StatusChip from '$lib/ui/StatusChip.svelte';

	// One format when every track has the same codec, or 'mixed' when they differ
	// (a display policy, not a ranking); a lossy one also has its bitrate.
	interface Props {
		format: string | null | undefined;
		/** Most common indexed-track bitrate, kbps. */
		bitrate?: number | null;
		size?: 'badge-xs' | 'badge-sm';
	}

	let { format, bitrate = null, size = 'badge-sm' }: Props = $props();

	type Tone = 'action' | 'neutral';

	const LOSSY_FORMATS = ['mp3', 'ogg', 'oga', 'opus', 'm4a', 'aac'];

	const config = $derived.by((): { label: string; tone: Tone } | null => {
		const f = (format ?? '').toLowerCase();
		if (!f) return null;
		const withBitrate = (label: string) =>
			LOSSY_FORMATS.includes(f) && bitrate ? `${label}-${Math.round(bitrate)}` : label;
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
