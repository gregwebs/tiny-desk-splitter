import { chromium } from 'playwright';
import * as fs from 'fs';
import * as path from 'path';

interface ScrapedData {
  setList: string[];
  musicians: string[];
  date?: string;
  storyTitle?: string;
  description?: string;
}

interface Musician {
  name: string;
  instruments: string[];
}

interface ConcertInfo {
  artist: string;
  source: string;
  show: string;
  date?: string;
  album?: string;
  description?: string;
  setList: {
    songNumber: number;
    title: string;
  }[];
  musicians: Musician[];
}

async function scrapeData(url: string): Promise<void> {
  // Launch the browser
  const browser = await chromium.launch();
  const page = await browser.newPage();
  
  try {
    // Navigate to the URL
    console.log(`Navigating to ${url}...`);
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    
    // Extract the artist name from the title
    const title = await page.title();
    const artistName = title.split(':')[0].trim();
    console.log(`Artist: ${artistName}`);
    
    // Wait for the storytext div to be available
    await page.waitForSelector('#storytext', { timeout: 10000 });
    
    // Look for the SET LIST, MUSICIANS sections, and date
    const data = await page.evaluate(() => {
      // Find the storytext container
      const storytext = document.querySelector('#storytext');
      if (!storytext) return { setList: [], musicians: [], date: undefined };
      
      // Look for the SET LIST and MUSICIANS paragraphs
      const paragraphs = storytext.querySelectorAll('p');
      let setListParagraph = null;
      let musiciansParagraph = null;
      
      // Get the first paragraph as description
      let description: string | undefined = undefined;
      if (paragraphs.length > 0) {
        const firstParagraph = paragraphs[0];
        if (firstParagraph && firstParagraph.textContent) {
          description = firstParagraph.textContent.trim();
        }
      }
      
      let description_done = false;
      for (const p of paragraphs) {
        if (!p.textContent) {
	  continue
	}
        if (p.textContent.includes('SET LIST')) {
          setListParagraph = p;
      	  description_done = true;
        }
        if (p.textContent.includes('MUSICIANS')) {
          musiciansParagraph = p;
      	  description_done = true;
        }
	if (!description_done) {
	  if (description) {
	    description += "\n\n"
	  } else {
	    description = "";
	  }
	  description += p.textContent
	}
      }
      
      // Extract date from dateblock
      let date: string | undefined = undefined;
      const dateBlock = document.querySelector('.dateblock');
      if (dateBlock) {
        const timeElement = dateBlock.querySelector('time');
        if (timeElement) {
          date = timeElement.getAttribute('datetime') || undefined;
        }
      }
      
      // Extract story title from storytitle div
      let storyTitle: string | undefined = undefined;
      const storyTitleDiv = document.querySelector('.storytitle h1');
      if (storyTitleDiv && storyTitleDiv.textContent) {
        storyTitle = storyTitleDiv.textContent.trim();
      }
      
      const result: { setList: string[], musicians: string[], date?: string, storyTitle?: string, description?: string } = { 
        setList: [], 
        musicians: [], 
        date,
        storyTitle,
        description
      };
      
      // Extract set list
      if (setListParagraph) {
        const nextElement = setListParagraph.nextElementSibling;
        if (nextElement && nextElement.tagName === 'UL') {
          const listItems = nextElement.querySelectorAll('li');
          result.setList = Array.from(listItems).map(li => {
            // Remove quotes if they exist and trim whitespace
            let text = li.textContent || '';
            // Remove both double quotes and single quotes from start and end
            text = text.trim().replace(/^["']/, '').replace(/["']$/, '');
            return text.trim();
          });
        }
      }
      
      // Extract musicians
      if (musiciansParagraph) {
        const nextElement = musiciansParagraph.nextElementSibling;
        if (nextElement && nextElement.tagName === 'UL') {
          const listItems = nextElement.querySelectorAll('li');
          result.musicians = Array.from(listItems).map(li => {
            let text = li.textContent || '';
            // Remove both double quotes and single quotes from start and end
            text = text.trim().replace(/^["']/, '').replace(/["']$/, '');
            return text.trim();
          });
        }
      }
      
      return result;
    });
    
    // Create output filename based on artist name
    const sanitizedArtistName = artistName.replace(/[^\w\s]/gi, '').replace(/\s+/g, '_').toLowerCase();
    const outputFileName = `${sanitizedArtistName}_info.json`;
    
    // Log results
    if (data.storyTitle) {
      console.log(`Story Title: ${data.storyTitle}`);
    } else {
      console.log('No story title found');
    }
    
    if (data.date) {
      console.log(`Date: ${data.date}`);
    } else {
      console.log('No date found');
    }
    
    if (!data.description) {
      console.log('No description found');
    }
    
    if (data.setList.length > 0) {
      console.log('\nSet list:');
      data.setList.forEach((song, index) => {
        console.log(`${index + 1}. ${song}`);
      });
    } else {
      console.log('No set list found');
    }
    
    if (data.musicians.length > 0) {
      console.log('\nMusicians:');
      data.musicians.forEach((musician, index) => {
        console.log(`${index + 1}. ${musician}`);
      });
    } else {
      console.log('No musicians list found');
    }
    
    // Parse musicians to separate name and instruments
    const parsedMusicians = data.musicians.map((musician, index) => {
      const parts = musician.split(':');
      if (parts.length === 2) {
        return {
          name: parts[0].trim(),
          instruments: parts[1].trim().split(", ")
        };
      } else {
        return {
          name: musician.trim(),
	  instruments: []
        };
      }
    });
    
    // Create JSON structure
    const concertInfo: ConcertInfo = {
      artist: artistName.trim(),
      source: url.trim(),
      date: data.date,
      album: data.storyTitle,
      description: data.description,
      setList: data.setList.map((song, index) => ({
        songNumber: index + 1,
        title: song.trim()
      })),
      musicians: parsedMusicians,
      show: "Tiny Desk Concerts"
    };
    
    // Write to file as JSON
    fs.writeFileSync(outputFileName, JSON.stringify(concertInfo, null, 2));
    console.log(`\nInformation saved to ${outputFileName}`);
    
  } catch (error) {
    console.error('Error scraping the data:', error);
  } finally {
    // Close the browser
    await browser.close();
  }
}

// Get URL from command line arguments
const url = process.argv[2];

if (!url) {
  console.error('Please provide a URL as an argument');
  console.log('Usage: npx ts-node scraper.ts <URL>');
  process.exit(1);
}

scrapeData(url)
  .catch(console.error);
