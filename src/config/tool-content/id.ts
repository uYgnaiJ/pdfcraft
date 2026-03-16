/**
 * Indonesian tool content for SEO
 * Falls back to English for now, to be expanded.
 */

import { ToolContent } from '@/types/tool';
import { toolContentEn } from './en';

/**
 * Indonesian tool content map
 */
export const toolContentId: Record<string, ToolContent> = {
  // Add specific Indonesian translations here
  // For now, we spread the English content and will gradually translate
  ...toolContentEn,
};
