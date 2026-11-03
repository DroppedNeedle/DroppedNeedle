<script lang="ts">
	// The album summary carries a normalized format for homogeneous albums, or the
	// literal 'mixed' when indexed tracks use more than one format (a display
	// policy, not a quality ranking). bitrate lives on per-track AudioQualityBadge.
	interface Props {
		format: string | null | undefined;
		size?: string;
	}

	let { format, size = 'badge-sm' }: Props = $props();

	const config = $derived.by(() => {
		const f = (format ?? '').toLowerCase();
		if (!f) return null;
		if (f === 'mixed') return { label: 'MIXED', cls: 'badge-ghost' };
		if (f === 'flac' || f === 'wav' || f === 'alac')
			return { label: f.toUpperCase(), cls: 'badge-success' };
		if (f === 'mp3') return { label: 'MP3', cls: 'badge-info' };
		if (f === 'ogg' || f === 'oga') return { label: 'OGG', cls: 'badge-ghost' };
		if (f === 'opus') return { label: 'OPUS', cls: 'badge-ghost' };
		if (f === 'm4a' || f === 'aac' || f === 'mp4') return { label: 'M4A', cls: 'badge-ghost' };
		return { label: f.toUpperCase(), cls: 'badge-ghost' };
	});
</script>

{#if config}
	<span class="badge {size} {config.cls} font-mono uppercase">{config.label}</span>
{/if}
