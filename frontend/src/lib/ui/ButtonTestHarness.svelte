<script lang="ts">
	import Button from './Button.svelte';

	interface Props {
		type?: 'button' | 'submit';
	}

	let { type }: Props = $props();

	let submitted = $state(0);
</script>

<form
	onsubmit={(event) => {
		event.preventDefault();
		submitted += 1;
	}}
>
	<input aria-label="Name" />
	<Button {type}>Save</Button>
</form>
<p>Submitted {submitted} times</p>
