<script lang="ts">
	import { FileX } from 'lucide-svelte';

	// A library album none of whose files is in the library now (missing,
	// in the recycle bin or excluded). It has nothing to play.
	interface Props {
		size?: 'xs' | 'sm';
		className?: string;
	}

	let { size = 'sm', className = '' }: Props = $props();
</script>

<span
	class="badge badge-warning badge-soft gap-1 {className}"
	class:badge-xs={size === 'xs'}
	class:badge-sm={size === 'sm'}
	title="None of this album's files is in the library"
>
	<FileX class={size === 'xs' ? 'h-2.5 w-2.5' : 'h-3 w-3'} aria-hidden="true" />
	No files
</span>
