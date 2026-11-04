<!--
	A labelled password input with a show and hide button, for signing in and
	choosing a password. A stored secret in the settings uses SecretField,
	which also keeps the server's mask.
	Replaces the password rows the sign-in and setup pages built from Field.
-->
<script lang="ts">
	import { Eye, EyeOff } from 'lucide-svelte';
	import Field from './Field.svelte';
	import IconButton from './IconButton.svelte';
	import Input from './Input.svelte';

	interface Props {
		label: string;
		value: string;
		help?: string;
		error?: string | null;
		placeholder?: string;
		autocomplete?: 'current-password' | 'new-password';
		required?: boolean;
		disabled?: boolean;
		/** Shows the value instead of dots. */
		shown?: boolean;
	}

	let {
		label,
		value = $bindable(''),
		help,
		error,
		placeholder,
		autocomplete = 'current-password',
		required = false,
		disabled = false,
		shown = $bindable(false)
	}: Props = $props();
</script>

<Field {label} {help} {error}>
	{#snippet children(control)}
		<div class="join w-full">
			<Input
				{...control}
				{label}
				type={shown ? 'text' : 'password'}
				class="join-item w-full"
				error={Boolean(error)}
				{placeholder}
				{required}
				{disabled}
				{autocomplete}
				bind:value
			/>
			<IconButton
				label="Show password"
				icon={shown ? EyeOff : Eye}
				variant="neutral"
				shape="square"
				class="join-item"
				aria-pressed={shown}
				{disabled}
				onclick={() => (shown = !shown)}
			/>
		</div>
	{/snippet}
</Field>
