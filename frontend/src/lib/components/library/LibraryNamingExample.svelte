<script lang="ts">
	// Where the saved import naming script puts an example track, as the
	// server rendered it.
	interface Props {
		example: string | null;
	}

	let { example }: Props = $props();
</script>

<div class="mt-1 break-all rounded-box bg-base-200 p-2 font-mono text-xs text-base-content/70">
	<span class="font-sans">Example:</span>
	{example ?? 'none'}
</div>
