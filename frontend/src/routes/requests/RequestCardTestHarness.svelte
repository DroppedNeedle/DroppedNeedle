<script lang="ts">
	import type { RequestKind } from '$lib/constants';
	import type { RequestItem } from '$lib/queries/requests/types';

	interface Props {
		item: RequestItem;
		selectable?: boolean;
		selected?: boolean;
		onselect?: (mbid: string, requestKind: RequestKind, selected: boolean) => void;
		oncancel?: (mbid: string, requestKind: RequestKind) => void;
	}

	let { item, selectable = false, selected = false, onselect, oncancel }: Props = $props();

	const kind = $derived(item.request_kind === 'track' ? 'track' : 'album');
	const title = $derived(
		item.request_kind === 'track' && item.track_title ? item.track_title : item.album_title
	);
</script>

<div data-testid="request-card-harness">
	<span>{title}</span>
	{#if selectable}
		<input
			type="checkbox"
			aria-label="Select {title}"
			checked={selected}
			onchange={(event) => onselect?.(item.musicbrainz_id, kind, event.currentTarget.checked)}
		/>
	{/if}
	{#if oncancel}
		<button onclick={() => oncancel?.(item.musicbrainz_id, kind)}>Cancel {title}</button>
	{/if}
</div>
