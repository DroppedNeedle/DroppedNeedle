<script lang="ts">
	import StatusChip from '$lib/ui/StatusChip.svelte';

	// The album summary carries a normalized format for homogeneous albums, or the
	// literal 'mixed' when indexed tracks use more than one format (a display
	// policy, not a quality ranking). bitrate lives on per-track AudioQualityBadge.
	interface Props {
		format: string | null | undefined;
		size?: 'badge-xs' | 'badge-sm';
	}

	let { format, size = 'badge-sm' }: Props = $props();

	type Tone = 'action' | 'info' | 'neutral';

	const config = $derived.by((): { label: string; tone: Tone } | null => {
		const f = (format ?? '').toLowerCase();
		if (!f) return null;
		if (f === 'mixed') return { label: 'MIXED', tone: 'neutral' };
		if (f === 'flac' || f === 'wav' || f === 'alac')
			return { label: f.toUpperCase(), tone: 'action' };
		if (f === 'mp3') return { label: 'MP3', tone: 'info' };
		if (f === 'ogg' || f === 'oga') return { label: 'OGG', tone: 'neutral' };
		if (f === 'opus') return { label: 'OPUS', tone: 'neutral' };
		if (f === 'm4a' || f === 'aac' || f === 'mp4') return { label: 'M4A', tone: 'neutral' };
		return { label: f.toUpperCase(), tone: 'neutral' };
	});
</script>

{#if config}
	<StatusChip tone={config.tone} size={size === 'badge-xs' ? 'xs' : 'sm'}>{config.label}</StatusChip
	>
{/if}
