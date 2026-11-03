<script lang="ts">
	interface Props {
		testId?: string;
	}

	let { testId = undefined }: Props = $props();
</script>

<img
	data-testid={testId}
	src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7"
	alt=""
/>
