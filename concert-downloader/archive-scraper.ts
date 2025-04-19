import { chromium } from 'playwright';
import * as fs from 'fs';
import * as path from 'path';

interface ConcertListing {
  title: string;
  url: string;
  date: string;
  teaser: string;
}

/**
 * Get the last day of a month
 */
function getLastDayOfMonth(year: number, month: number): number {
  // The date constructor uses 0-based months (0 = January), so subtract 1
  // The day 0 of next month is the last day of current month
  return new Date(year, month, 0).getDate();
}

async function scrapeArchive(year: string, month: string, day?: string): Promise<void> {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  
  try {
    // If day is not provided, use the last day of the month
    let dayValue: string;
    if (!day) {
      const lastDay = getLastDayOfMonth(parseInt(year), parseInt(month));
      dayValue = lastDay.toString().padStart(2, '0');
      console.log(`No day specified, using last day of month: ${dayValue}`);
    } else {
      dayValue = day;
    }
    
    // Construct the URL with date
    const url = `https://www.npr.org/series/tiny-desk-concerts/archive?date=${month}-${dayValue}-${year}`;
    console.log(`Navigating to ${url}...`);
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    
    // Wait for the content to load
    await page.waitForSelector('#main-section', { timeout: 15000 });
    
    // Extract concert listings
    const concerts = await page.evaluate(() => {
      const listings: ConcertListing[] = [];
      
      // Query all article elements which contain concert listings
      const articles = document.querySelectorAll('article.item');
      
      articles.forEach(article => {
        // Get the title element
        const titleElement = article.querySelector('.title a');
        
        // Get the time element with datetime attribute
        const timeElement = article.querySelector('time');
        
        // Get the teaser element
        const teaserElement = article.querySelector('.teaser');
        
        if (titleElement && timeElement && teaserElement) {
          const title = titleElement.textContent?.trim() || '';
          const url = titleElement.getAttribute('href') || '';
          
          // Get the datetime attribute instead of text content
          const dateAttr = timeElement.getAttribute('datetime') || '';
          
          // Extract teaser text, removing the date portion
          // First, get the full teaser text
          const fullTeaserText = teaserElement.textContent?.trim() || '';
          
          // Get the date text that we want to remove
          const dateText = timeElement.textContent?.trim() || '';
          
          // Remove the date text from the teaser
          let cleanTeaser = fullTeaserText;
          if (dateText) {
            // Replace the date text and any following dots or bullets
            cleanTeaser = fullTeaserText.replace(dateText, '').replace(/^[•\s]+/, '').trim();
          }
          
          listings.push({
            title,
            url,
            date: dateAttr,
            teaser: cleanTeaser
          });
        }
      });
      
      return listings;
    });
    
    // Format date string for display
    const displayDate = day 
      ? `${month}/${day}/${year}` 
      : `${month}/${dayValue}/${year} (last day of month)`;
    console.log(`Found ${concerts.length} Tiny Desk Concerts for ${displayDate}`);
    
    // Create output filename
    const outputFileName = day
      ? `listing_${year}_${month}_${day}.json`
      : `listing_${year}_${month}.json`;
    
    // Format for console output and JSON
    if (concerts.length > 0) {
      console.log('\nConcert Listings:');
      concerts.forEach((concert, index) => {
        console.log(`${index + 1}. ${concert.title} (${concert.date})`);
        // Display the teaser, but truncate long teasers with ellipsis
        if (concert.teaser) {
          const truncatedTeaser = concert.teaser.length > 100 
            ? concert.teaser.substring(0, 100) + '...'
            : concert.teaser;
          console.log(`   ${truncatedTeaser}`);
        }
        console.log(`   URL: ${concert.url}`);
        console.log('-'.repeat(50));
      });
      
      // Write to file as JSON
      fs.writeFileSync(outputFileName, JSON.stringify(concerts, null, 2));
      console.log(`\nListings saved to ${outputFileName}`);
    } else {
      console.log('No Tiny Desk Concerts found for this period');
    }
    
  } catch (error) {
    console.error('Error scraping the archive:', error);
  } finally {
    await browser.close();
  }
}

// Get year, month, and optional day from command line arguments
const year = process.argv[2];
const month = process.argv[3];
const day = process.argv[4];

if (!year || !month) {
  console.error('Please provide year and month as arguments');
  console.log('Usage: npx ts-node archive-scraper.ts <YEAR> <MONTH> [DAY]');
  console.log('Example: npx ts-node archive-scraper.ts 2023 01');
  console.log('Example with day: npx ts-node archive-scraper.ts 2023 01 15');
  process.exit(1);
}

// Validate year, month, and day format
const yearRegex = /^\d{4}$/;
const monthRegex = /^(0[1-9]|1[0-2])$/;
const dayRegex = /^(0[1-9]|[12][0-9]|3[01])$/;

if (!yearRegex.test(year)) {
  console.error('Year must be in YYYY format (e.g., 2023)');
  process.exit(1);
}

if (!monthRegex.test(month)) {
  console.error('Month must be in MM format (e.g., 01 for January)');
  process.exit(1);
}

if (day && !dayRegex.test(day)) {
  console.error('Day must be in DD format (e.g., 01 for the 1st)');
  process.exit(1);
}

// Validate that day is valid for the given month and year
if (day) {
  const dayNum = parseInt(day);
  const lastDayOfMonth = getLastDayOfMonth(parseInt(year), parseInt(month));
  if (dayNum < 1 || dayNum > lastDayOfMonth) {
    console.error(`Invalid day: ${day}. For ${month}/${year}, days must be between 1 and ${lastDayOfMonth}`);
    process.exit(1);
  }
}

scrapeArchive(year, month, day)
  .catch(console.error);
