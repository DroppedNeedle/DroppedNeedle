import type { components } from '$lib/api/v3/openapi';
// Plugin wire types come from the generated contract.

export const PLUGIN_SECRET_MASK = 'plugin****';

export type PluginSettingFieldInfo = components['schemas']['PluginSettingFieldInfo'];

export type PluginInfo = components['schemas']['PluginInfo'];

export type PluginListResponse = components['schemas']['PluginListResponse'];
