<!--
	Asks before unsaved settings are lost: on a tab switch that goes through
	`confirmLeave`, on a link or Back inside the app, and on a reload or
	closing the tab (the browser's own prompt). Mount once on a page that
	holds settings sections.
-->
<script lang="ts">
	import { beforeNavigate, goto } from '$app/navigation';
	import ConfirmDialog from './ConfirmDialog.svelte';
	import { confirmLeave, hasUnsavedSettings, unsavedLeave } from './settingsSection.svelte';

	beforeNavigate((navigation) => {
		if (!hasUnsavedSettings()) return;
		navigation.cancel();
		// A reload, a closed tab or an outside link: cancelling makes the
		// browser ask.
		if (navigation.willUnload || !navigation.to) return;
		const { type, delta } = navigation;
		const url = navigation.to.url;
		confirmLeave(() => {
			if (type === 'popstate' && delta) history.go(delta);
			else void goto(url);
		});
	});
</script>

<ConfirmDialog
	open={unsavedLeave.pending}
	title="Discard unsaved changes?"
	message="Your changes here have not been saved."
	confirmLabel="Discard"
	cancelLabel="Keep editing"
	tone="danger"
	onconfirm={() => unsavedLeave.confirm()}
	oncancel={() => unsavedLeave.cancel()}
/>
